// SPDX-License-Identifier: GPL-3.0-or-later

//! What is known to be on disk.
//!
//! [`Ranges`] tracks confirmed byte intervals. [`Checkpoint`] stores them next
//! to the part file so an interrupted download can continue.
//!
//! Durability rule (SOP 6.3): a checkpoint may only list bytes that were synced
//! to disk before the checkpoint was written. The caller enforces the order
//! "write data, sync data, save checkpoint"; this crate makes the save atomic.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Checkpoint layout version. A file with another version is ignored.
const FORMAT: u32 = 1;
/// A checkpoint larger than this is treated as corrupt rather than read into memory.
const MAX_CHECKPOINT_BYTES: u64 = 1024 * 1024;

/// A set of half-open byte intervals `[start, end)`, kept sorted, disjoint and merged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ranges {
    spans: Vec<(u64, u64)>,
}

impl Ranges {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a set from stored spans. Returns `None` unless they are sorted,
    /// non-empty, disjoint, non-adjacent and inside `[0, total)`.
    pub fn from_spans(spans: Vec<(u64, u64)>, total: u64) -> Option<Self> {
        let mut previous_end = None;
        for &(start, end) in &spans {
            if start >= end || end > total || previous_end.is_some_and(|p| start <= p) {
                return None;
            }
            previous_end = Some(end);
        }
        Some(Self { spans })
    }

    /// Adds `[start, end)`, merging with anything it touches.
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let (mut start, mut end) = (start, end);
        let mut merged = Vec::with_capacity(self.spans.len() + 1);
        let mut placed = false;
        for &(a, b) in &self.spans {
            if b < start {
                merged.push((a, b));
            } else if a > end {
                if !placed {
                    merged.push((start, end));
                    placed = true;
                }
                merged.push((a, b));
            } else {
                start = start.min(a);
                end = end.max(b);
            }
        }
        if !placed {
            merged.push((start, end));
        }
        self.spans = merged;
    }

    /// Total number of bytes covered.
    pub fn covered(&self) -> u64 {
        self.spans.iter().map(|(a, b)| b - a).sum()
    }

    /// True when the set is exactly `[0, total)`.
    pub fn is_complete(&self, total: u64) -> bool {
        if total == 0 {
            self.spans.is_empty()
        } else {
            self.spans == [(0, total)]
        }
    }

    /// The parts of `[0, total)` that are not covered, in order.
    pub fn missing(&self, total: u64) -> Vec<(u64, u64)> {
        let mut gaps = Vec::new();
        let mut cursor = 0;
        for &(a, b) in &self.spans {
            if a > cursor {
                gaps.push((cursor, a.min(total)));
            }
            cursor = cursor.max(b);
        }
        if cursor < total {
            gaps.push((cursor, total));
        }
        gaps
    }

    /// The spans, sorted.
    pub fn spans(&self) -> &[(u64, u64)] {
        &self.spans
    }
}

/// Saved progress of one download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    format: u32,
    /// Identifies the resource without storing its address. See the engine for how it is derived.
    pub identity: String,
    /// Size of the complete file.
    pub total: u64,
    /// Strong `ETag` of the version these bytes belong to.
    pub etag: String,
    done: Vec<(u64, u64)>,
}

impl Checkpoint {
    /// Creates a checkpoint for the given confirmed intervals.
    pub fn new(identity: String, total: u64, etag: String, done: &Ranges) -> Self {
        Self { format: FORMAT, identity, total, etag, done: done.spans().to_vec() }
    }

    /// The confirmed intervals.
    pub fn ranges(&self) -> Ranges {
        Ranges { spans: self.done.clone() }
    }

    /// Writes the checkpoint so that a crash leaves either the old file or the
    /// new one, never a half-written mix: write a sibling file, sync it, rename
    /// it over the target.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = Path::new(&tmp);
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        let mut file = File::create(tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(tmp, path)
    }

    /// Reads a checkpoint. Returns `None` when the file is missing, too large,
    /// unreadable, of another version, or internally inconsistent. A checkpoint
    /// that cannot be trusted is the same as no checkpoint.
    pub fn load(path: &Path) -> Option<Self> {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_CHECKPOINT_BYTES {
            return None;
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        File::open(path).ok()?.take(MAX_CHECKPOINT_BYTES).read_to_end(&mut bytes).ok()?;
        let checkpoint: Checkpoint = serde_json::from_slice(&bytes).ok()?;
        if checkpoint.format != FORMAT || checkpoint.total == 0 || checkpoint.etag.is_empty() {
            return None;
        }
        Ranges::from_spans(checkpoint.done.clone(), checkpoint.total)?;
        Some(checkpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(spans: &[(u64, u64)]) -> Ranges {
        let mut r = Ranges::new();
        for &(a, b) in spans {
            r.insert(a, b);
        }
        r
    }

    #[test]
    fn inserts_merge_overlapping_and_adjacent_spans() {
        assert_eq!(ranges(&[(10, 20), (0, 5)]).spans(), [(0, 5), (10, 20)]);
        assert_eq!(ranges(&[(0, 5), (5, 10)]).spans(), [(0, 10)]);
        assert_eq!(ranges(&[(0, 5), (10, 20), (3, 12)]).spans(), [(0, 20)]);
        assert_eq!(ranges(&[(0, 5), (10, 20), (30, 40), (4, 31)]).spans(), [(0, 40)]);
        assert_eq!(ranges(&[(5, 5), (9, 3)]).spans(), []);
    }

    #[test]
    fn inserting_the_same_span_twice_changes_nothing() {
        let once = ranges(&[(0, 5), (10, 20)]);
        let mut twice = once.clone();
        twice.insert(10, 20);
        twice.insert(2, 4);
        assert_eq!(once, twice);
    }

    #[test]
    fn covered_missing_and_complete_agree() {
        let r = ranges(&[(10, 20), (30, 40)]);
        assert_eq!(r.covered(), 20);
        assert_eq!(r.missing(50), [(0, 10), (20, 30), (40, 50)]);
        assert!(!r.is_complete(50));

        let full = ranges(&[(0, 25), (25, 50)]);
        assert!(full.is_complete(50));
        assert!(full.missing(50).is_empty());
        assert!(Ranges::new().is_complete(0));
        assert_eq!(Ranges::new().missing(7), [(0, 7)]);
    }

    #[test]
    fn missing_plus_covered_always_equals_total() {
        // Every subset of 8 fixed blocks: gaps and spans must tile [0, 80) exactly.
        for mask in 0u32..256 {
            let mut r = Ranges::new();
            for bit in 0..8 {
                if mask & (1 << bit) != 0 {
                    r.insert(bit * 10, bit * 10 + 10);
                }
            }
            let gaps: u64 = r.missing(80).iter().map(|(a, b)| b - a).sum();
            assert_eq!(gaps + r.covered(), 80, "mask {mask:#b}");
        }
    }

    #[test]
    fn stored_spans_are_validated() {
        assert!(Ranges::from_spans(vec![(0, 5), (10, 20)], 20).is_some());
        assert!(Ranges::from_spans(vec![(10, 20), (0, 5)], 20).is_none(), "unsorted");
        assert!(Ranges::from_spans(vec![(0, 5), (5, 9)], 20).is_none(), "adjacent, should have been merged");
        assert!(Ranges::from_spans(vec![(0, 5), (3, 9)], 20).is_none(), "overlapping");
        assert!(Ranges::from_spans(vec![(5, 5)], 20).is_none(), "empty");
        assert!(Ranges::from_spans(vec![(0, 21)], 20).is_none(), "past the end");
    }

    #[test]
    fn checkpoint_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.qrxpart.state");
        let saved = Checkpoint::new("id".into(), 100, "\"v1\"".into(), &ranges(&[(0, 10), (50, 60)]));

        saved.save(&path).unwrap();
        saved.save(&path).unwrap(); // replacing an existing checkpoint must work

        assert_eq!(Checkpoint::load(&path), Some(saved.clone()));
        assert_eq!(saved.ranges().spans(), [(0, 10), (50, 60)]);
        assert!(!dir.path().join("file.qrxpart.state.tmp").exists());
    }

    #[test]
    fn untrustworthy_checkpoints_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        assert_eq!(Checkpoint::load(&path), None, "missing");

        for bad in [
            "",
            "not json",
            "{\"format\":1",
            r#"{"format":2,"identity":"id","total":100,"etag":"e","done":[]}"#,
            r#"{"format":1,"identity":"id","total":100,"etag":"e","done":[[50,10]]}"#,
            r#"{"format":1,"identity":"id","total":100,"etag":"e","done":[[0,500]]}"#,
            r#"{"format":1,"identity":"id","total":100,"etag":"","done":[]}"#,
            r#"{"format":1,"identity":"id","total":0,"etag":"e","done":[]}"#,
        ] {
            fs::write(&path, bad).unwrap();
            assert_eq!(Checkpoint::load(&path), None, "input {bad:?}");
        }
    }
}
