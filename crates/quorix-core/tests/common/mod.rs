// SPDX-License-Identifier: GPL-3.0-or-later

//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use quorix_core::{download, Algorithm, CancelToken, Completed, DownloadRequest, Error, Outcome, Target};

/// A request suited to tests: no system proxy, short timeouts, no retries
/// unless the test turns them on, and fast backoff when it does.
pub fn request(url: String, dir: &Path) -> DownloadRequest {
    let mut r = DownloadRequest::new(url, Target::Directory(dir.to_path_buf()));
    r.http.use_system_proxy = false;
    r.http.read_timeout = Duration::from_secs(5);
    r.retry.max_retries = 0;
    r.adaptive = false;
    r.connections = 4;
    r.retry.base_delay = Duration::from_millis(10);
    r.retry.max_delay = Duration::from_millis(40);
    r
}

/// Runs to the end and returns the outcome.
pub async fn run(r: &DownloadRequest) -> Result<Outcome, Error> {
    download(r, &CancelToken::new(), &|_| {}).await
}

/// Runs to the end and expects a completed file.
pub async fn complete(r: &DownloadRequest) -> Completed {
    run(r).await.expect("download should succeed").completed().expect("download should complete, not pause")
}

/// Names of everything in `dir`, to prove what a download left behind.
pub fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

pub fn sha256_of(data: &[u8]) -> String {
    let mut h = quorix_integrity::Hasher::new(Algorithm::Sha256);
    h.update(data);
    h.finalize_hex()
}
