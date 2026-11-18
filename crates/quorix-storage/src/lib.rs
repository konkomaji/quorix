// SPDX-License-Identifier: GPL-3.0-or-later

//! Disk side of a download: safe file names, the temporary part file and the
//! final move into place.
//!
//! Rules enforced here (SOP 7):
//! - Unfinished data lives in `<name>.qrxpart` next to the destination, never at the destination.
//! - An existing destination is never replaced unless the caller asked for it.
//! - A file name taken from the network can never escape the chosen directory.
//! - Writes are positional, so several workers can fill one file without sharing a cursor.
//! - Data is synced before the part file is moved into place.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Suffix of an unfinished download.
pub const PART_SUFFIX: &str = ".qrxpart";
/// Suffix of the checkpoint that belongs to a part file.
pub const STATE_SUFFIX: &str = ".qrxpart.state";

const MAX_NAME_CHARS: usize = 200;
const FALLBACK_NAME: &str = "download";
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4",
    "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Storage failures.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The destination already exists and overwriting was not requested.
    #[error("{} already exists", .0.display())]
    DestinationExists(PathBuf),
    /// The disk has no space left.
    #[error("the disk is full")]
    DiskFull,
    /// The destination path cannot be used.
    #[error("{0}")]
    InvalidPath(String),
    /// Any other I/O failure.
    #[error("{0}")]
    Io(#[source] io::Error),
}

impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::StorageFull {
            StorageError::DiskFull
        } else {
            StorageError::Io(e)
        }
    }
}

/// Turns an untrusted name (from a URL or a response header) into a plain file
/// name that is safe on Windows, macOS and Linux.
///
/// Only the last path component is kept, so `../../evil.txt` becomes `evil.txt`.
pub fn sanitize_file_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).find(|part| !part.trim().is_empty()).unwrap_or("");

    let mut name: String = last
        .chars()
        .map(|c| match c {
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(MAX_NAME_CHARS)
        .collect();

    // Windows drops trailing dots and spaces, which would make two names collide.
    name = name.trim().trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        return FALLBACK_NAME.to_string();
    }

    let stem = name.split('.').next().unwrap_or("");
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        name.insert(0, '_');
    }
    name
}

fn sibling(dest: &Path, suffix: &str) -> Result<PathBuf, StorageError> {
    let name = dest.file_name().ok_or_else(|| StorageError::InvalidPath(format!("{} has no file name", dest.display())))?;
    let mut full = name.to_os_string();
    full.push(suffix);
    Ok(dest.with_file_name(full))
}

/// Path of the checkpoint that belongs to `dest`.
pub fn state_path_for(dest: &Path) -> Result<PathBuf, StorageError> {
    sibling(dest, STATE_SUFFIX)
}

/// Runs blocking file work off the async threads.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, StorageError> + Send + 'static) -> Result<T, StorageError> {
    tokio::task::spawn_blocking(work).await.unwrap_or_else(|e| Err(StorageError::Io(io::Error::other(e))))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A download in progress on disk. Cheap to clone; all clones write to the same file.
#[derive(Debug, Clone)]
pub struct PartFile {
    file: Arc<File>,
    part: PathBuf,
    state: PathBuf,
    dest: PathBuf,
}

impl PartFile {
    /// Creates a fresh part file for `dest`, discarding anything left by an earlier attempt.
    ///
    /// When `size` is known the file is given its full length at once so workers
    /// can write anywhere in it. On Windows it is marked sparse first; without
    /// that, the first write far into the file would make the system fill
    /// everything before it with zeros.
    ///
    /// The file is created with "must not exist" semantics after the old one is
    /// removed, so a symbolic link planted at the part path is never followed.
    pub async fn create(dest: &Path, overwrite: bool, size: Option<u64>) -> Result<Self, StorageError> {
        let (part, state, dest) = (sibling(dest, PART_SUFFIX)?, sibling(dest, STATE_SUFFIX)?, dest.to_path_buf());
        blocking(move || {
            if !overwrite && dest.exists() {
                return Err(StorageError::DestinationExists(dest));
            }
            if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            remove_if_present(&state)?;
            remove_if_present(&part)?;
            let file = OpenOptions::new().read(true).write(true).create_new(true).open(&part)?;
            if let Some(size) = size {
                let _ = mark_sparse(&file); // an optimisation; the download is correct without it
                file.set_len(size)?;
            }
            Ok(Self { file: Arc::new(file), part, state, dest })
        })
        .await
    }

    /// Opens the part file left by an earlier run, for resuming.
    ///
    /// Returns `None` when there is nothing usable: no part file, not a regular
    /// file, or not exactly `expected_len` bytes long.
    pub async fn reopen(dest: &Path, overwrite: bool, expected_len: u64) -> Result<Option<Self>, StorageError> {
        let (part, state, dest) = (sibling(dest, PART_SUFFIX)?, sibling(dest, STATE_SUFFIX)?, dest.to_path_buf());
        blocking(move || {
            if !overwrite && dest.exists() {
                return Err(StorageError::DestinationExists(dest));
            }
            match std::fs::symlink_metadata(&part) {
                Ok(meta) if meta.is_file() && meta.len() == expected_len => {}
                _ => return Ok(None),
            }
            let file = OpenOptions::new().read(true).write(true).open(&part)?;
            Ok(Some(Self { file: Arc::new(file), part, state, dest }))
        })
        .await
    }

    /// Writes `data` at `offset` and hands the buffer back, emptied, so the
    /// caller can reuse its allocation.
    pub async fn write_at(&self, offset: u64, mut data: Vec<u8>) -> Result<Vec<u8>, StorageError> {
        let file = Arc::clone(&self.file);
        blocking(move || {
            write_all_at(&file, &data, offset)?;
            data.clear();
            Ok(data)
        })
        .await
    }

    /// Reads exactly `len` bytes at `offset`.
    pub async fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>, StorageError> {
        let file = Arc::clone(&self.file);
        blocking(move || {
            let mut data = vec![0u8; len];
            read_exact_at(&file, &mut data, offset)?;
            Ok(data)
        })
        .await
    }

    /// Forces written data to the disk. A checkpoint may only list bytes written before this returned.
    pub async fn sync_data(&self) -> Result<(), StorageError> {
        let file = Arc::clone(&self.file);
        blocking(move || Ok(file.sync_data()?)).await
    }

    /// Current size of the part file.
    pub async fn size(&self) -> Result<u64, StorageError> {
        let file = Arc::clone(&self.file);
        blocking(move || Ok(file.metadata()?.len())).await
    }

    /// Path of the part file, for reading it back during verification.
    pub fn part_path(&self) -> &Path {
        &self.part
    }

    /// Path of the checkpoint that belongs to this part file.
    pub fn state_path(&self) -> &Path {
        &self.state
    }

    /// Final location of the file.
    pub fn destination(&self) -> &Path {
        &self.dest
    }

    /// Syncs everything and moves the part file to its destination.
    ///
    /// On failure the part file and its checkpoint are removed and the
    /// destination is left as it was.
    pub async fn finalize(self, overwrite: bool) -> Result<PathBuf, StorageError> {
        let Self { file, part, state, dest } = self;
        blocking(move || {
            let moved = file.sync_all().map_err(StorageError::from).and_then(|()| {
                drop(file); // Windows cannot rename or delete an open file.
                move_into_place(&part, &dest, overwrite)
            });
            let _ = remove_if_present(&state);
            match moved {
                Ok(()) => Ok(dest),
                Err(e) => {
                    let _ = remove_if_present(&part);
                    Err(e)
                }
            }
        })
        .await
    }

    /// Deletes the part file and its checkpoint. Use this when the data must not be reused.
    pub async fn discard(self) -> Result<(), StorageError> {
        let Self { file, part, state, .. } = self;
        blocking(move || {
            drop(file);
            let _ = remove_if_present(&state);
            Ok(remove_if_present(&part)?)
        })
        .await
    }
}

#[cfg(windows)]
fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        match file.seek_write(data, offset) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                data = &data[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buffer.is_empty() {
        match file.seek_read(buffer, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buffer = &mut buffer[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, offset)
}

#[cfg(unix)]
fn write_all_at(file: &File, data: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, data, offset)
}

/// Marks the file sparse so unwritten regions take no disk space and need no zero-filling.
#[cfg(windows)]
#[allow(unsafe_code)]
fn mark_sparse(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let mut returned = 0u32;
    // SAFETY:
    // - The handle comes from `file`, which is borrowed for the whole call, so it is open and valid.
    // - FSCTL_SET_SPARSE accepts a missing input buffer (meaning "set"); both buffers are null with length 0,
    //   so the system reads and writes no memory through them.
    // - `returned` is a live u32 the call may write to.
    // - The handle was opened for synchronous I/O, so a null OVERLAPPED pointer is correct.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn mark_sparse(_file: &File) -> io::Result<()> {
    Ok(()) // files extended with set_len are already sparse on the file systems these platforms use
}

/// Moves `part` to `dest`. Without `overwrite` this must not replace a file
/// that appeared after the download started.
fn move_into_place(part: &Path, dest: &Path, overwrite: bool) -> Result<(), StorageError> {
    if overwrite {
        return Ok(std::fs::rename(part, dest)?);
    }
    // A hard link fails when the name is taken, which makes the check and the
    // move one atomic step. Not every file system supports links (FAT, some
    // network shares), so fall back to check-then-rename there.
    match std::fs::hard_link(part, dest) {
        Ok(()) => {
            let _ = std::fs::remove_file(part);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(StorageError::DestinationExists(dest.to_path_buf())),
        Err(_) => {
            if dest.exists() {
                return Err(StorageError::DestinationExists(dest.to_path_buf()));
            }
            Ok(std::fs::rename(part, dest)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_ordinary_names() {
        assert_eq!(sanitize_file_name("ubuntu-26.04-desktop-amd64.iso"), "ubuntu-26.04-desktop-amd64.iso");
        assert_eq!(sanitize_file_name("résumé final.pdf"), "résumé final.pdf");
    }

    #[test]
    fn strips_directories_and_traversal() {
        assert_eq!(sanitize_file_name("../../evil.txt"), "evil.txt");
        assert_eq!(sanitize_file_name("..\\..\\windows\\system32\\evil.dll"), "evil.dll");
        assert_eq!(sanitize_file_name("/etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C:\\Users\\me\\file.bin"), "file.bin");
    }

    #[test]
    fn replaces_forbidden_characters() {
        assert_eq!(sanitize_file_name("a:b*c?d\"e<f>g|h.txt"), "a_b_c_d_e_f_g_h.txt");
        assert_eq!(sanitize_file_name("line\nbreak\t.txt"), "line_break_.txt");
    }

    #[test]
    fn falls_back_when_nothing_usable_remains() {
        for raw in ["", "   ", "..", ".", "/", "\\\\", "...  "] {
            assert_eq!(sanitize_file_name(raw), "download", "input {raw:?}");
        }
    }

    #[test]
    fn guards_reserved_windows_names_and_trailing_dots() {
        assert_eq!(sanitize_file_name("CON"), "_CON");
        assert_eq!(sanitize_file_name("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_file_name("report.pdf. . ."), "report.pdf");
    }

    #[test]
    fn limits_length() {
        assert_eq!(sanitize_file_name(&"a".repeat(500)).chars().count(), MAX_NAME_CHARS);
    }

    #[tokio::test]
    async fn out_of_order_writes_land_at_their_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("sub").join("file.bin");
        let part = PartFile::create(&dest, false, Some(11)).await.unwrap();
        assert_eq!(part.size().await.unwrap(), 11, "the full length is reserved up front");

        let other = part.clone();
        let (a, b) = tokio::join!(part.write_at(6, b"world".to_vec()), other.write_at(0, b"hello ".to_vec()));
        assert!(a.unwrap().is_empty() && b.unwrap().is_empty(), "buffers come back emptied for reuse");
        assert!(!dest.exists(), "destination must not exist before finalize");

        drop(other);
        let path = part.finalize(false).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
        assert_eq!(std::fs::read_dir(dir.path().join("sub")).unwrap().count(), 1, "no part or state file may remain");
    }

    #[tokio::test]
    async fn a_far_write_into_a_large_reserved_file_is_fast() {
        // 8 GiB reserved, one write near the end. Without sparse allocation this
        // would zero-fill gigabytes and take far longer than the limit below.
        let dir = tempfile::tempdir().unwrap();
        let size = 8u64 * 1024 * 1024 * 1024;
        let part = PartFile::create(&dir.path().join("big.bin"), false, Some(size)).await.unwrap();

        let started = std::time::Instant::now();
        part.write_at(size - 4, b"tail".to_vec()).await.unwrap();
        part.sync_data().await.unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "took {:?}", started.elapsed());

        part.discard().await.unwrap();
    }

    #[tokio::test]
    async fn reads_back_exactly_what_was_written_at_an_offset() {
        let dir = tempfile::tempdir().unwrap();
        let part = PartFile::create(&dir.path().join("file.bin"), false, Some(12)).await.unwrap();
        part.write_at(4, b"abcdef".to_vec()).await.unwrap();

        assert_eq!(part.read_at(4, 6).await.unwrap(), b"abcdef");
        assert_eq!(part.read_at(0, 4).await.unwrap(), [0, 0, 0, 0], "bytes never written read as zero");
        assert!(part.read_at(8, 100).await.is_err(), "a read past the end is an error, not a short answer");
        part.discard().await.unwrap();
    }

    #[tokio::test]
    async fn refuses_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        std::fs::write(&dest, b"keep me").unwrap();

        assert!(matches!(PartFile::create(&dest, false, None).await.unwrap_err(), StorageError::DestinationExists(_)));
        assert!(matches!(PartFile::reopen(&dest, false, 3).await.unwrap_err(), StorageError::DestinationExists(_)));
        assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");
    }

    #[tokio::test]
    async fn destination_that_appears_mid_download_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");

        let part = PartFile::create(&dest, false, None).await.unwrap();
        part.write_at(0, b"new".to_vec()).await.unwrap();
        std::fs::write(&dest, b"appeared later").unwrap();

        let err = part.finalize(false).await.unwrap_err();
        assert!(matches!(err, StorageError::DestinationExists(_)));
        assert_eq!(std::fs::read(&dest).unwrap(), b"appeared later");
        assert!(!dir.path().join("file.bin.qrxpart").exists(), "part file must be cleaned up");
    }

    #[tokio::test]
    async fn overwrite_replaces_and_discard_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        std::fs::write(&dest, b"old").unwrap();

        let part = PartFile::create(&dest, true, None).await.unwrap();
        part.write_at(0, b"new".to_vec()).await.unwrap();
        part.finalize(true).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");

        let other = dir.path().join("other.bin");
        let part = PartFile::create(&other, false, Some(4)).await.unwrap();
        std::fs::write(part.state_path(), b"{}").unwrap();
        part.discard().await.unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "only file.bin may remain");
    }

    #[tokio::test]
    async fn reopen_accepts_only_a_part_file_of_the_expected_length() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        assert!(PartFile::reopen(&dest, false, 8).await.unwrap().is_none(), "nothing to reopen");

        let part = PartFile::create(&dest, false, Some(8)).await.unwrap();
        part.write_at(0, b"abcd".to_vec()).await.unwrap();
        drop(part); // as after a crash: the part file stays

        assert!(PartFile::reopen(&dest, false, 9).await.unwrap().is_none(), "wrong length");
        let again = PartFile::reopen(&dest, false, 8).await.unwrap().expect("usable part file");
        again.write_at(4, b"efgh".to_vec()).await.unwrap();
        assert_eq!(std::fs::read(again.finalize(false).await.unwrap()).unwrap(), b"abcdefgh");
    }

    #[tokio::test]
    async fn create_replaces_a_stale_part_and_its_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        std::fs::write(dir.path().join("file.bin.qrxpart"), b"stale data from another version").unwrap();
        std::fs::write(dir.path().join("file.bin.qrxpart.state"), b"stale").unwrap();

        let part = PartFile::create(&dest, false, Some(3)).await.unwrap();
        assert_eq!(part.size().await.unwrap(), 3);
        assert!(!part.state_path().exists());
        part.discard().await.unwrap();
    }
}
