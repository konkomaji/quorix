// SPDX-License-Identifier: GPL-3.0-or-later

//! Several network adapters at once.
//!
//! The tests use two loopback addresses, `127.0.0.1` and `127.0.0.2`. Each is a
//! separate local address, so the test server sees two different clients, the
//! same way a real server sees a computer with Ethernet and Wi-Fi.

mod common;

use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{listing, request, run};
use quorix_core::{download, CancelToken, DownloadRequest, Error, Event};
use quorix_testserver::{body, TestServer};

const SIZE: usize = 1_000_003;
const NAME: &str = "1000003";

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

/// An address that no adapter on this computer owns (reserved for documentation, RFC 5737).
/// Binding to it fails at once, like an adapter that was unplugged.
fn missing_adapter() -> IpAddr {
    ip("192.0.2.77")
}

fn pieces(url: String, dir: &Path, addresses: &[IpAddr]) -> DownloadRequest {
    let mut r = request(url, dir);
    r.piece_size = Some(64 * 1024);
    r.local_addresses = addresses.to_vec();
    r
}

/// Runs to completion and returns the adapters the engine gave up on.
async fn run_recording_paths(r: &DownloadRequest) -> (Vec<IpAddr>, quorix_core::Completed) {
    let down = Mutex::new(Vec::new());
    let done = download(r, &CancelToken::new(), &|event| {
        if let Event::PathDown(address) = event {
            down.lock().unwrap().push(address);
        }
    })
    .await
    .expect("download should succeed")
    .completed()
    .expect("download should complete");
    (down.into_inner().unwrap(), done)
}

#[tokio::test]
async fn connections_are_spread_over_every_adapter() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let r = pieces(server.url(&format!("/range/{SIZE}")), dir.path(), &[ip("127.0.0.1"), ip("127.0.0.2")]);

    let (down, done) = run_recording_paths(&r).await;

    assert_eq!(server.peer_addresses(), [ip("127.0.0.1"), ip("127.0.0.2")], "the server must see both adapters");
    assert!(down.is_empty(), "{down:?}");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [NAME]);
}

#[tokio::test]
async fn without_named_adapters_the_system_chooses_one() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let r = pieces(server.url(&format!("/range/{SIZE}")), dir.path(), &[]);

    let (_, done) = run_recording_paths(&r).await;

    assert_eq!(server.peer_addresses().len(), 1);
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
}

#[tokio::test]
async fn two_adapters_pass_a_limit_that_the_server_applies_to_each_address() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let size = 2_400_000; // 4 seconds at the server's limit of 600 kB/s for one address
    let r = pieces(server.url(&format!("/range-peraddr/{size}")), dir.path(), &[ip("127.0.0.1"), ip("127.0.0.2")]);

    let started = Instant::now();
    let (_, done) = run_recording_paths(&r).await;
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_millis(3000), "two addresses should need about 2 seconds, took {elapsed:?}");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(size));
}

#[tokio::test]
async fn one_adapter_is_held_to_the_limit_for_its_address() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let size = 2_400_000;
    let r = pieces(server.url(&format!("/range-peraddr/{size}")), dir.path(), &[ip("127.0.0.1")]);

    // The control for the test above: with one address the limit holds, whatever the connection count.
    let started = Instant::now();
    let (_, done) = run_recording_paths(&r).await;
    let elapsed = started.elapsed();

    assert!(elapsed > Duration::from_millis(3300), "one address cannot beat the limit, took {elapsed:?}");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(size));
}

#[tokio::test]
async fn an_adapter_that_cannot_connect_is_dropped_and_the_download_completes() {
    for addresses in [[ip("127.0.0.1"), missing_adapter()], [missing_adapter(), ip("127.0.0.1")]] {
        let server = TestServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let r = pieces(server.url(&format!("/range/{SIZE}")), dir.path(), &addresses);

        let (down, done) = run_recording_paths(&r).await;

        assert_eq!(down, [missing_adapter()], "order {addresses:?}: the dead adapter must be reported once");
        assert_eq!(server.peer_addresses(), [ip("127.0.0.1")], "order {addresses:?}");
        assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE), "order {addresses:?}");
        assert_eq!(listing(dir.path()), [NAME]);
    }
}

#[tokio::test]
async fn when_no_adapter_can_connect_the_error_says_so() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let r = pieces(server.url(&format!("/range/{SIZE}")), dir.path(), &[missing_adapter(), ip("192.0.2.78")]);

    let err = run(&r).await.unwrap_err();

    assert!(matches!(err, Error::Connect(_)), "{err:?}");
    assert!(err.is_retryable());
    assert_eq!(server.hits(), 0);
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn the_same_adapter_named_twice_is_used_once() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let r = pieces(server.url(&format!("/range/{SIZE}")), dir.path(), &[ip("127.0.0.2"), ip("127.0.0.2")]);

    let (_, done) = run_recording_paths(&r).await;

    assert_eq!(server.peer_addresses(), [ip("127.0.0.2")]);
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
}
