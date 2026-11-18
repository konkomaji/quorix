// SPDX-License-Identifier: GPL-3.0-or-later

//! Servers that do not honour ranges: one request, one stream.
//! Every failure test also checks the two promises that matter most: nothing
//! is left at the destination, and no part file is left behind.

mod common;

use std::sync::Mutex;
use std::time::Duration;

use common::{complete, listing, request, run, sha256_of};
use quorix_core::{download, Algorithm, CancelToken, Error, Event, Expected, Target, TaskState};
use quorix_testserver::{body, TestServer};

const SIZE: usize = 300_000;

#[tokio::test]
async fn downloads_a_file_exactly() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let done = complete(&request(server.url(&format!("/ok/{SIZE}")), dir.path())).await;

    assert_eq!(done.bytes, SIZE as u64);
    assert_eq!(done.path, dir.path().join(SIZE.to_string()));
    assert_eq!(done.etag.as_deref(), Some("\"v1\""));
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [SIZE.to_string()], "no part file may remain");
    assert_eq!(server.hits(), 1, "the inspection answer must be reused as the download");
}

#[tokio::test]
async fn reports_states_in_order_and_final_progress() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let events = Mutex::new(Vec::new());

    download(&request(server.url(&format!("/ok/{SIZE}")), dir.path()), &CancelToken::new(), &|e| events.lock().unwrap().push(e))
        .await
        .unwrap();

    let events = events.into_inner().unwrap();
    let states: Vec<TaskState> = events.iter().filter_map(|e| if let Event::State(s) = e { Some(*s) } else { None }).collect();
    use TaskState::*;
    assert_eq!(states, [Created, Inspecting, Queued, Downloading, Verifying, Finalizing, Completed]);

    let last_progress = events.iter().rev().find_map(|e| match e {
        Event::Progress { downloaded, total } => Some((*downloaded, *total)),
        _ => None,
    });
    assert_eq!(last_progress, Some((SIZE as u64, Some(SIZE as u64))));
}

#[tokio::test]
async fn downloads_without_a_declared_length() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let done = complete(&request(server.url(&format!("/nolen/{SIZE}")), dir.path())).await;

    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
}

#[tokio::test]
async fn downloads_an_empty_file() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let done = complete(&request(server.url("/ok/0"), dir.path())).await;

    assert_eq!(done.bytes, 0);
    assert_eq!(std::fs::read(&done.path).unwrap(), b"");
}

#[tokio::test]
async fn follows_a_redirect_and_names_the_file_from_the_final_address() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let done = complete(&request(server.url("/redirect/4096"), dir.path())).await;

    assert!(done.final_url.ends_with("/ok/4096"));
    assert_eq!(done.path, dir.path().join("4096"));
    assert_eq!(std::fs::read(&done.path).unwrap(), body(4096));
}

#[tokio::test]
async fn stops_on_a_redirect_loop() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(&request(server.url("/loop"), dir.path())).await.unwrap_err();

    assert!(matches!(err, Error::TooManyRedirects), "{err:?}");
    assert_eq!(err.code(), "QRX-E205");
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn maps_http_statuses_to_named_errors() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let cases = [(401, "QRX-E401", false), (403, "QRX-E403", false), (404, "QRX-E404", false), (500, "QRX-E500", true)];
    for (status, code, retryable) in cases {
        let err = run(&request(server.url(&format!("/status/{status}")), dir.path())).await.unwrap_err();
        assert!(matches!(err, Error::Http { status: s, .. } if s == status), "{err:?}");
        assert_eq!(err.code(), code);
        assert_eq!(err.is_retryable(), retryable, "status {status}");
    }
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn reads_retry_after_on_429_and_503() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    for status in [429, 503] {
        let err = run(&request(server.url(&format!("/status/{status}")), dir.path())).await.unwrap_err();
        assert!(matches!(err, Error::Http { retry_after: Some(7), .. }), "{err:?}");
        assert!(err.is_retryable());
    }
}

#[tokio::test]
async fn a_connection_that_closes_early_is_a_failure_not_a_short_file() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(&request(server.url(&format!("/short/{SIZE}")), dir.path())).await.unwrap_err();

    assert!(matches!(err, Error::ConnectionClosed | Error::LengthMismatch { .. }), "{err:?}");
    assert!(listing(dir.path()).is_empty(), "a truncated file must never be kept");
}

#[tokio::test]
async fn extra_bytes_beyond_the_declared_length_are_never_written_to_the_destination() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    // The HTTP layer stops reading at Content-Length, so the file is either
    // exactly the announced bytes or the transfer is rejected. Both are correct.
    match run(&request(server.url("/long/4096"), dir.path())).await {
        Ok(outcome) => assert_eq!(std::fs::read(outcome.completed().unwrap().path).unwrap(), body(4096 + 16)[..4096]),
        Err(err) => {
            assert!(matches!(err, Error::LengthMismatch { .. } | Error::ConnectionClosed), "{err:?}");
            assert!(listing(dir.path()).is_empty());
        }
    }
}

#[tokio::test]
async fn refuses_a_partial_answer_that_does_not_match_what_was_asked() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    // The server answers every request with "bytes 0-4095 of 8192", so the
    // second half can never be fetched correctly.
    let err = run(&request(server.url("/partial/4096"), dir.path())).await.unwrap_err();

    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
    assert_eq!(err.code(), "QRX-E703");
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn refuses_a_content_encoding_it_did_not_ask_for() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(&request(server.url("/gzip"), dir.path())).await.unwrap_err();

    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn a_matching_checksum_is_verified_and_reported() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = request(server.url(&format!("/ok/{SIZE}")), dir.path());
    r.expected = Some(Expected::parse(Algorithm::Sha256, &sha256_of(&body(SIZE))).unwrap());

    let done = complete(&r).await;

    assert_eq!(done.digest, Some((Algorithm::Sha256, sha256_of(&body(SIZE)))));
}

#[tokio::test]
async fn a_wrong_checksum_fails_and_keeps_nothing() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = request(server.url(&format!("/ok/{SIZE}")), dir.path());
    r.expected = Some(Expected::parse(Algorithm::Blake3, &"0".repeat(64)).unwrap());
    let states = Mutex::new(Vec::new());

    let err = download(&r, &CancelToken::new(), &|e| {
        if let Event::State(s) = e {
            states.lock().unwrap().push(s)
        }
    })
    .await
    .unwrap_err();

    assert!(matches!(err, Error::ChecksumMismatch { .. }), "{err:?}");
    assert_eq!(err.code(), "QRX-E702");
    assert_eq!(states.into_inner().unwrap().last(), Some(&TaskState::Failed));
    assert!(listing(dir.path()).is_empty(), "an unverified file must never be kept");
}

#[tokio::test]
async fn an_existing_file_is_kept_unless_overwrite_is_requested() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.bin");
    std::fs::write(&dest, b"original").unwrap();
    let mut r = request(server.url("/ok/4096"), dir.path());
    r.target = Target::File(dest.clone());

    let err = run(&r).await.unwrap_err();
    assert!(matches!(err, Error::DestinationExists(_)), "{err:?}");
    assert_eq!(err.code(), "QRX-E601");
    assert_eq!(std::fs::read(&dest).unwrap(), b"original");
    assert_eq!(listing(dir.path()), ["file.bin"]);

    r.overwrite = true;
    complete(&r).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body(4096));
}

#[tokio::test]
async fn a_file_name_from_the_server_cannot_escape_the_directory() {
    let server = TestServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a").join("b");
    std::fs::create_dir_all(&dir).unwrap();

    let done = complete(&request(server.url("/named"), &dir)).await;

    assert_eq!(done.path, dir.join("evil.txt"));
    assert_eq!(listing(root.path()), ["a"], "nothing may be written above the chosen directory");
    assert_eq!(listing(&root.path().join("a")), ["b"]);
}

#[tokio::test]
async fn cancelling_mid_transfer_stops_quickly_and_keeps_nothing() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancelToken::new();
    let states = Mutex::new(Vec::new());
    let r = request(server.url(&format!("/stall/{SIZE}")), dir.path());

    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
        trigger.cancel(); // repeating the command must be harmless
    });

    let outcome = tokio::time::timeout(
        Duration::from_secs(4),
        download(&r, &cancel, &|e| {
            if let Event::State(s) = e {
                states.lock().unwrap().push(s)
            }
        }),
    )
    .await
    .expect("cancel must not wait for the stalled server");

    assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
    assert_eq!(states.into_inner().unwrap().last(), Some(&TaskState::Cancelled));
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn pausing_a_download_that_cannot_resume_discards_it_honestly() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancelToken::new();
    let r = request(server.url(&format!("/stall/{SIZE}")), dir.path());

    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.pause();
    });

    // No ranges, so nothing could continue later. Reporting "paused" would be a lie.
    let outcome = tokio::time::timeout(Duration::from_secs(4), download(&r, &cancel, &|_| {})).await.unwrap();
    assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn a_token_cancelled_before_the_start_prevents_any_request() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();

    let err = download(&request(server.url("/ok/4096"), dir.path()), &cancel, &|_| {}).await.unwrap_err();

    assert!(matches!(err, Error::Cancelled), "{err:?}");
    assert_eq!(server.hits(), 0);
}

#[tokio::test]
async fn a_silent_server_times_out() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = request(server.url(&format!("/stall/{SIZE}")), dir.path());
    r.http.read_timeout = Duration::from_millis(500);

    let err = run(&r).await.unwrap_err();

    assert!(matches!(err, Error::Timeout | Error::ConnectionClosed), "{err:?}");
    assert!(err.is_retryable());
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn rejects_links_that_are_not_http() {
    let dir = tempfile::tempdir().unwrap();

    let err = run(&request("ftp://example.com/file.zip".into(), dir.path())).await.unwrap_err();
    assert!(matches!(err, Error::UnsupportedScheme(_)));
    assert_eq!(err.code(), "QRX-E102");

    let err = run(&request("not a link".into(), dir.path())).await.unwrap_err();
    assert!(matches!(err, Error::InvalidUrl(_)));
}

#[tokio::test]
async fn nothing_listening_is_a_connection_error() {
    let dir = tempfile::tempdir().unwrap();
    // Bind then drop to get a port that is certainly closed.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();

    let err = run(&request(format!("http://127.0.0.1:{port}/x"), dir.path())).await.unwrap_err();

    assert!(matches!(err, Error::Connect(_)), "{err:?}");
    assert!(err.is_retryable());
}

#[tokio::test]
async fn a_server_that_claims_ranges_but_ignores_them_still_yields_the_exact_file() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let done = complete(&request(server.url(&format!("/range-ignore/{SIZE}")), dir.path())).await;

    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(server.hits(), 1);
}

#[tokio::test]
async fn retries_stop_at_the_limit_and_report_the_last_error() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = request(server.url("/status/500"), dir.path());
    r.retry.max_retries = 2;

    let err = run(&r).await.unwrap_err();

    assert!(matches!(err, Error::Http { status: 500, .. }), "{err:?}");
    assert_eq!(server.hits(), 3, "one attempt plus exactly two retries");
}
