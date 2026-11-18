// SPDX-License-Identifier: GPL-3.0-or-later

//! The two mechanisms that adapt a transfer while it runs: the connection
//! count that follows measured speed, and hedged requests for slow pieces.
//! Each test also proves the file is exact: speed must never cost correctness.

mod common;

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{listing, request};
use quorix_core::{download, CancelToken, DownloadRequest, Event};
use quorix_testserver::{body, TestServer};

const SIZE: usize = 1_000_003;
const NAME: &str = "1000003";

fn pieces(url: String, dir: &Path) -> DownloadRequest {
    let mut r = request(url, dir);
    r.piece_size = Some(64 * 1024);
    r
}

/// Runs to completion and returns the connection counts the engine reported, in order.
async fn run_recording_connections(r: &DownloadRequest) -> (Vec<usize>, quorix_core::Completed) {
    let seen = Mutex::new(Vec::new());
    let done = download(r, &CancelToken::new(), &|event| {
        if let Event::Connections(n) = event {
            seen.lock().unwrap().push(n);
        }
    })
    .await
    .expect("download should succeed")
    .completed()
    .expect("download should complete");
    (seen.into_inner().unwrap(), done)
}

#[tokio::test]
async fn connections_are_added_while_each_one_brings_more_speed() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let size = 5 * 1024 * 1024;
    let mut r = pieces(server.url(&format!("/range-slow/{size}")), dir.path());
    r.adaptive = true;
    r.connections = 16;

    // The server limits each connection, so twice the connections is twice the speed.
    let (changes, done) = run_recording_connections(&r).await;

    assert!(changes.iter().any(|&n| n >= 8), "the engine should have grown past its start of 4: {changes:?}");
    assert!(changes.windows(2).all(|w| w[1] > w[0]), "only growth is expected here: {changes:?}");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(size));
}

#[tokio::test]
async fn connections_that_bring_no_speed_are_given_back() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let size = 4 * 1024 * 1024;
    let mut r = pieces(server.url(&format!("/range-shared/{size}")), dir.path());
    r.adaptive = true;
    r.connections = 16;

    // The server has one limit for all connections together. More connections cannot help.
    let (changes, done) = run_recording_connections(&r).await;

    assert_eq!(changes, [8, 4], "one trial of 8, then back to 4, and never 16");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(size));
}

#[tokio::test]
async fn a_fixed_connection_count_is_not_changed_by_measurements() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let size = 2 * 1024 * 1024;
    let mut r = pieces(server.url(&format!("/range-slow/{size}")), dir.path());
    r.connections = 2; // `request` sets adaptive = false

    let (changes, done) = run_recording_connections(&r).await;

    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(size));
}

#[tokio::test]
async fn a_piece_on_a_slow_connection_is_finished_by_a_fast_one() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = pieces(server.url(&format!("/range-lag/{SIZE}")), dir.path());
    r.hedge_after = Duration::from_secs(60); // only the "much slower than me" rule may trigger

    // The first connection crawls: alone it needs about 6 seconds for its piece.
    let started = Instant::now();
    let (_, done) = run_recording_connections(&r).await;
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(3), "the slow piece should have been hedged, took {elapsed:?}");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [NAME]);
    assert_eq!(server.hits(), SIZE.div_ceil(64 * 1024) as u32 + 1, "one request per piece, plus exactly one hedge");
}

#[tokio::test]
async fn a_piece_on_a_silent_connection_is_finished_by_another() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = pieces(server.url(&format!("/range-stall1/{SIZE}")), dir.path());
    r.hedge_after = Duration::from_millis(300);
    r.http.read_timeout = Duration::from_secs(30); // far longer than the test: a timeout must not be what saves it

    let started = Instant::now();
    let (_, done) = run_recording_connections(&r).await;
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(3), "the stalled piece should have been hedged, took {elapsed:?}");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [NAME]);
}

#[tokio::test]
async fn hedged_bytes_are_never_counted_twice() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let r = pieces(server.url(&format!("/range-lag/{SIZE}")), dir.path());
    let highest = Mutex::new(0u64);

    download(&r, &CancelToken::new(), &|event| {
        if let Event::Progress { downloaded, .. } = event {
            let mut top = highest.lock().unwrap();
            *top = (*top).max(downloaded);
        }
    })
    .await
    .unwrap();

    assert_eq!(*highest.lock().unwrap(), SIZE as u64, "progress must end at the file size and never pass it");
}

#[tokio::test]
async fn without_a_strong_etag_no_piece_is_fetched_twice() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let size: usize = 400_000;
    let mut r = pieces(server.url(&format!("/range-weak/{size}")), dir.path());
    r.hedge_after = Duration::from_millis(1); // every piece looks stalled at once

    let (_, done) = run_recording_connections(&r).await;

    // Two writers on one range are only safe when a strong ETag proves they hold the same bytes.
    assert_eq!(server.hits(), size.div_ceil(64 * 1024) as u32, "exactly one request per piece");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(size));
}
