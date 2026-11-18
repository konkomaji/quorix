// SPDX-License-Identifier: GPL-3.0-or-later

//! What is known to be on disk.
//!
//! [`Ranges`] tracks confirmed byte intervals. [`Checkpoint`] stores them next
//! to the part file so an interrupted download can continue.
//!
//! Durability rule (SOP 6.3): a checkpoint may only list bytes that were synced
//! to disk before the checkpoint was written. The caller enforces the order
//! "write data, sync data, save checkpoint"; this crate makes the save atomic.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Checkpoint layout version. A file with another version is ignored.
const FORMAT: u32 = 2;
/// A checkpoint larger than this is treated as corrupt rather than read into memory.
const MAX_CHECKPOINT_BYTES: u64 = 8 * 1024 * 1024;

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
    /// Returns how many of those bytes were not covered before.
    pub fn insert(&mut self, start: u64, end: u64) -> u64 {
        if start >= end {
            return 0;
        }
        let before = self.covered();
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
        self.covered() - before
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

    /// True when every byte of `[start, end)` is covered.
    pub fn contains(&self, start: u64, end: u64) -> bool {
        start >= end || self.spans.iter().any(|&(a, b)| a <= start && end <= b)
    }

    /// The spans, sorted.
    pub fn spans(&self) -> &[(u64, u64)] {
        &self.spans
    }
}

/// Byte range `[start, end)` of block `index` in a file of `total` bytes cut
/// into blocks of `block_size`. The last block may be shorter. A block past the
/// end of the file is empty.
pub fn block_range(index: u64, block_size: u64, total: u64) -> (u64, u64) {
    let start = index.saturating_mul(block_size).min(total);
    (start, start.saturating_add(block_size).min(total))
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
    /// Size of the blocks the file is cut into for hashing.
    block_size: u64,
    /// BLAKE3 hash of each complete block, as `(block index, lowercase hex)`, sorted by index.
    blocks: Vec<(u64, String)>,
}

impl Checkpoint {
    /// Creates a checkpoint for the given confirmed intervals and block hashes.
    pub fn new(identity: String, total: u64, etag: String, done: &Ranges, block_size: u64, blocks: &BTreeMap<u64, String>) -> Self {
        let blocks = blocks.iter().map(|(index, hash)| (*index, hash.clone())).collect();
        Self { format: FORMAT, identity, total, etag, done: done.spans().to_vec(), block_size, blocks }
    }

    /// The confirmed intervals.
    pub fn ranges(&self) -> Ranges {
        Ranges { spans: self.done.clone() }
    }

    /// Size of the blocks the hashes refer to.
    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    /// Hash of each complete block, sorted by block index.
    pub fn blocks(&self) -> &[(u64, String)] {
        &self.blocks
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
        if checkpoint.format != FORMAT || checkpoint.total == 0 || checkpoint.etag.is_empty() || checkpoint.block_size == 0 {
            return None;
        }
        let done = Ranges::from_spans(checkpoint.done.clone(), checkpoint.total)?;

        // Every hash must name a real block that the checkpoint claims is fully on disk.
        let mut previous = None;
        for (index, hash) in &checkpoint.blocks {
            let (start, end) = block_range(*index, checkpoint.block_size, checkpoint.total);
            let well_formed = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if start >= end || !well_formed || !done.contains(start, end) || previous.is_some_and(|p| *index <= p) {
                return None;
            }
            previous = Some(*index);
        }
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
    fn insert_reports_only_the_bytes_that_were_new() {
        let mut r = Ranges::new();
        assert_eq!(r.insert(10, 20), 10);
        assert_eq!(r.insert(10, 20), 0, "a repeat adds nothing");
        assert_eq!(r.insert(15, 30), 10, "only the part beyond 20 is new");
        assert_eq!(r.insert(0, 40), 20, "0..10 and 30..40");
        assert_eq!(r.insert(7, 7), 0);
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

    fn hashes(indexes: &[u64]) -> BTreeMap<u64, String> {
        indexes.iter().map(|i| (*i, format!("{i:064x}"))).collect()
    }

    #[test]
    fn contains_needs_one_span_to_hold_the_whole_range() {
        let r = ranges(&[(0, 10), (20, 30)]);
        assert!(r.contains(0, 10) && r.contains(3, 7) && r.contains(20, 30));
        assert!(!r.contains(5, 25), "the gap 10..20 is not covered");
        assert!(!r.contains(29, 31));
        assert!(r.contains(15, 15), "an empty range is always covered");
    }

    #[test]
    fn blocks_tile_the_file_and_the_last_one_may_be_short() {
        assert_eq!(block_range(0, 10, 25), (0, 10));
        assert_eq!(block_range(2, 10, 25), (20, 25));
        assert_eq!(block_range(3, 10, 25), (25, 25), "past the end: empty");
        assert_eq!(block_range(u64::MAX, u64::MAX, 25), (25, 25), "no overflow");
    }

    #[test]
    fn checkpoint_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.qrxpart.state");
        let saved = Checkpoint::new("id".into(), 100, "\"v1\"".into(), &ranges(&[(0, 30), (50, 60)]), 10, &hashes(&[0, 2, 5]));

        saved.save(&path).unwrap();
        saved.save(&path).unwrap(); // replacing an existing checkpoint must work

        assert_eq!(Checkpoint::load(&path), Some(saved.clone()));
        assert_eq!(saved.ranges().spans(), [(0, 30), (50, 60)]);
        assert_eq!(saved.block_size(), 10);
        assert_eq!(saved.blocks().iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 2, 5]);
        assert!(!dir.path().join("file.qrxpart.state.tmp").exists());
    }

    #[test]
    fn untrustworthy_checkpoints_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        assert_eq!(Checkpoint::load(&path), None, "missing");

        let hash = "a".repeat(64);
        let json = |format: u32, total: u64, etag: &str, done: &str, block_size: u64, blocks: &str| {
            format!(
                r#"{{"format":{format},"identity":"id","total":{total},"etag":"{etag}","done":{done},"block_size":{block_size},"blocks":{blocks}}}"#
            )
        };
        let good = json(2, 100, "e", "[[0,30]]", 10, &format!(r#"[[0,"{hash}"],[2,"{hash}"]]"#));
        fs::write(&path, &good).unwrap();
        assert!(Checkpoint::load(&path).is_some(), "control: the well-formed one loads");

        let bad = [
            ("empty file", String::new()),
            ("not json", "not json".to_string()),
            ("cut short", good[..good.len() / 2].to_string()),
            ("old format", json(1, 100, "e", "[[0,30]]", 10, "[]")),
            ("reversed span", json(2, 100, "e", "[[50,10]]", 10, "[]")),
            ("span past the end", json(2, 100, "e", "[[0,500]]", 10, "[]")),
            ("no etag", json(2, 100, "", "[[0,30]]", 10, "[]")),
            ("empty file size", json(2, 0, "e", "[]", 10, "[]")),
            ("zero block size", json(2, 100, "e", "[[0,30]]", 0, "[]")),
            ("hash for a block that is not on disk", json(2, 100, "e", "[[0,30]]", 10, &format!(r#"[[3,"{hash}"]]"#))),
            ("hash for a block that is only partly on disk", json(2, 100, "e", "[[0,25]]", 10, &format!(r#"[[2,"{hash}"]]"#))),
            ("hash for a block past the end", json(2, 100, "e", "[[0,100]]", 10, &format!(r#"[[10,"{hash}"]]"#))),
            ("hash too short", json(2, 100, "e", "[[0,30]]", 10, r#"[[0,"abc"]]"#)),
            ("hash not lowercase hex", json(2, 100, "e", "[[0,30]]", 10, &format!(r#"[[0,"{}"]]"#, "G".repeat(64)))),
            ("blocks out of order", json(2, 100, "e", "[[0,30]]", 10, &format!(r#"[[2,"{hash}"],[0,"{hash}"]]"#))),
            ("block listed twice", json(2, 100, "e", "[[0,30]]", 10, &format!(r#"[[1,"{hash}"],[1,"{hash}"]]"#))),
        ];
        for (what, text) in bad {
            fs::write(&path, text).unwrap();
            assert_eq!(Checkpoint::load(&path), None, "{what}");
        }
    }
}
