// SPDX-License-Identifier: GPL-3.0-or-later

//! Servers that honour ranges: several connections, resume, retries.
//! The standard of proof is the same throughout: the finished file is compared
//! byte for byte with what the server holds, and the directory is checked for leftovers.

mod common;

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use common::{complete, listing, request, run, sha256_of};
use quorix_core::{download, Algorithm, CancelToken, DownloadRequest, Error, Event, Expected, Outcome, TaskState};
use quorix_testserver::{body, body_of_version, TestServer};

const SIZE: usize = 1_000_003; // prime, so no piece size divides it evenly
const NAME: &str = "1000003";

/// A request cut into many small pieces so several connections are really used.
fn pieces(url: String, dir: &Path) -> DownloadRequest {
    let mut r = request(url, dir);
    r.piece_size = Some(64 * 1024);
    r.connections = 4;
    r.checkpoint_interval = Duration::from_millis(50);
    r
}

/// Starts a download and pauses it once some bytes are on disk.
/// The route stops sending at 40% of the file, so the download cannot finish before the pause.
async fn pause_midway(server: &TestServer, route: &str, dir: &Path) -> (u64, u64) {
    let cancel = CancelToken::new();
    let r = pieces(server.url(&format!("/{route}/{SIZE}")), dir);

    let outcome = download(&r, &cancel, &|event| {
        if matches!(event, Event::Progress { downloaded, .. } if downloaded > 200_000) {
            cancel.pause();
        }
    })
    .await;

    match outcome {
        Ok(Outcome::Paused { downloaded, total }) => (downloaded, total),
        other => panic!("expected a paused download, got {other:?}"),
    }
}

/// Runs the same address again, at full speed, and expects a complete file.
async fn finish(server: &TestServer, dir: &Path) -> quorix_core::Completed {
    server.stop_throttling();
    complete(&pieces(server.url(&format!("/range-hold/{SIZE}")), dir)).await
}

#[tokio::test]
async fn several_connections_produce_the_exact_file() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let connections = Mutex::new(0);

    let done = download(&pieces(server.url(&format!("/range/{SIZE}")), dir.path()), &CancelToken::new(), &|event| {
        if let Event::Started { connections: n, .. } = event {
            *connections.lock().unwrap() = n;
        }
    })
    .await
    .unwrap()
    .completed()
    .unwrap();

    assert_eq!(done.bytes, SIZE as u64);
    assert_eq!(done.resumed, 0);
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [NAME], "no part file or checkpoint may remain");
    assert_eq!(*connections.lock().unwrap(), 4);
    assert_eq!(server.hits(), SIZE.div_ceil(64 * 1024) as u32, "one request per piece and none wasted on inspection");
}

#[tokio::test]
async fn every_piece_size_and_connection_count_gives_the_same_file() {
    let server = TestServer::start().await;
    for (piece, connections) in [(1, 3), (7, 16), (4096, 1), (65_536, 2), (999_999, 4), (5_000_000, 8)] {
        let dir = tempfile::tempdir().unwrap();
        let size = if piece < 100 { 997 } else { SIZE }; // tiny pieces on a tiny file, to keep the test quick
        let mut r = pieces(server.url(&format!("/range/{size}")), dir.path());
        r.piece_size = Some(piece);
        r.connections = connections;

        let done = complete(&r).await;

        assert_eq!(std::fs::read(&done.path).unwrap(), body(size), "piece size {piece}, {connections} connections");
    }
}

#[tokio::test]
async fn a_checksum_is_verified_across_pieces_written_out_of_order() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = pieces(server.url(&format!("/range/{SIZE}")), dir.path());
    r.expected = Some(Expected::parse(Algorithm::Sha256, &sha256_of(&body(SIZE))).unwrap());

    let done = complete(&r).await;

    assert_eq!(done.digest, Some((Algorithm::Sha256, sha256_of(&body(SIZE)))));
}

#[tokio::test]
async fn a_shifted_content_range_is_refused_and_nothing_is_kept() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let err = run(&pieces(server.url(&format!("/range-badcr/{SIZE}")), dir.path())).await.unwrap_err();

    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
    assert!(listing(dir.path()).is_empty(), "bytes from a mislabelled range must never be kept");
}

#[tokio::test]
async fn answers_that_stop_halfway_are_finished_by_asking_again() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = pieces(server.url(&format!("/range-half/{SIZE}")), dir.path());
    r.retry.max_retries = 3;

    let done = complete(&r).await;

    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [NAME]);
}

#[tokio::test]
async fn a_server_that_allows_one_connection_is_served_with_one() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let size = 200_000;
    let mut r = pieces(server.url(&format!("/range-one/{size}")), dir.path());
    r.piece_size = Some(20_000);

    // Extra connections get 403. They must step back, not fail the download.
    let done = complete(&r).await;

    assert_eq!(std::fs::read(&done.path).unwrap(), body(size));
}

#[tokio::test]
async fn temporary_503_answers_are_retried_and_reported_as_retrying() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut r = pieces(server.url(&format!("/range-503/{SIZE}")), dir.path());
    r.connections = 1; // one worker, so a 503 cannot be solved by stepping back
    r.retry.max_retries = 3;
    let states = Mutex::new(Vec::new());

    let done = download(&r, &CancelToken::new(), &|event| {
        if let Event::State(state) = event {
            states.lock().unwrap().push(state);
        }
    })
    .await
    .unwrap()
    .completed()
    .unwrap();

    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    let states = states.into_inner().unwrap();
    assert!(states.contains(&TaskState::Retrying), "{states:?}");
    assert_eq!(states.last(), Some(&TaskState::Completed));
}

#[tokio::test]
async fn pause_keeps_progress_and_the_next_run_continues_from_it() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let (downloaded, total) = pause_midway(&server, "range-hold", dir.path()).await;
    assert_eq!(total, SIZE as u64);
    assert!(downloaded > 0 && downloaded < total);
    assert_eq!(
        listing(dir.path()),
        [format!("{NAME}.qrxpart"), format!("{NAME}.qrxpart.state")],
        "partial data and checkpoint, nothing at the destination"
    );

    let hits_before = server.hits();
    let done = finish(&server, dir.path()).await;

    // Saved data is reused in whole blocks of 64 KiB, each checked against its hash.
    // Bytes of a block that was still incomplete at the pause are downloaded again.
    assert!(done.resumed > 0 && done.resumed <= downloaded, "resumed {} of {downloaded} saved", done.resumed);
    assert_eq!(done.resumed % (64 * 1024), 0, "only whole verified blocks are reused");
    assert!(downloaded - done.resumed < 4 * 2 * 64 * 1024, "at most the incomplete block of each connection is lost");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [NAME]);
    let refetched = (server.hits() - hits_before) as u64 * 64 * 1024;
    assert!(refetched < total, "the second run must not fetch the whole file again");
}

#[tokio::test]
async fn a_file_that_changed_on_the_server_is_never_mixed_with_old_data() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    pause_midway(&server, "range-hold", dir.path()).await;

    server.set_version(2); // same address, same size, different content
    let done = finish(&server, dir.path()).await;

    assert_eq!(done.resumed, 0, "old bytes belong to another version and must not be reused");
    assert_eq!(std::fs::read(&done.path).unwrap(), body_of_version(SIZE, 2));
}

#[tokio::test]
async fn a_change_during_the_download_stops_it_and_keeps_nothing() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    // Not the holding route: new requests must keep coming, so one of them meets the changed file.
    let r = pieces(server.url(&format!("/range-slow/{SIZE}")), dir.path());

    let outcome = download(&r, &CancelToken::new(), &|event| {
        if matches!(event, Event::Progress { downloaded, .. } if downloaded > 100_000) {
            server.set_version(2);
        }
    })
    .await;

    assert!(matches!(outcome, Err(Error::ResourceChanged)), "{outcome:?}");
    assert!(listing(dir.path()).is_empty(), "a file made of two versions must never be kept");
}

#[tokio::test]
async fn a_crash_mid_download_is_recovered_from_the_last_checkpoint() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let r = pieces(server.url(&format!("/range-hold/{SIZE}")), dir.path());

    // Dropping the future is the closest a test gets to killing the process:
    // no cleanup code runs, only what is already on disk survives.
    {
        let underway = tokio::sync::Notify::new();
        let never_stopped = CancelToken::new();
        let on_event = |event| {
            if matches!(event, Event::Progress { downloaded, .. } if downloaded > 150_000) {
                underway.notify_one();
            }
        };
        let running = download(&r, &never_stopped, &on_event);
        tokio::pin!(running);
        tokio::select! {
            outcome = &mut running => panic!("the download should still have been running: {outcome:?}"),
            _ = async {
                underway.notified().await;
                tokio::time::sleep(Duration::from_millis(250)).await; // long enough for a checkpoint to be saved
            } => {}
        }
    } // the download is dropped here, mid-transfer
    tokio::time::sleep(Duration::from_millis(100)).await; // let in-flight writes land
    assert_eq!(listing(dir.path()), [format!("{NAME}.qrxpart"), format!("{NAME}.qrxpart.state")], "after the crash");

    let done = finish(&server, dir.path()).await;

    assert!(done.resumed > 0, "checkpointed bytes must be reused");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [NAME]);
}

#[tokio::test]
async fn a_damaged_checkpoint_or_part_file_means_a_clean_restart() {
    let state = format!("{NAME}.qrxpart.state");
    let part = format!("{NAME}.qrxpart");
    type Damage = fn(&Path, &str, &str);
    let damages: [(&str, Damage); 4] = [
        ("garbage in the checkpoint", |d, s, _| std::fs::write(d.join(s), b"\x00\xffnot json").unwrap()),
        ("checkpoint claims more than the file holds", |d, s, _| {
            let text = std::fs::read_to_string(d.join(s)).unwrap();
            let cut = text.find("\"done\"").unwrap();
            std::fs::write(d.join(s), format!("{}\"done\":[[0,99999999]]}}", &text[..cut])).unwrap()
        }),
        ("part file truncated", |d, _, p| std::fs::OpenOptions::new().write(true).open(d.join(p)).unwrap().set_len(1000).unwrap()),
        ("part file missing", |d, _, p| std::fs::remove_file(d.join(p)).unwrap()),
    ];

    for (what, damage) in damages {
        // A fresh server each time: finishing removes the speed limit, and the next pause needs it back.
        let server = TestServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        pause_midway(&server, "range-hold", dir.path()).await;
        damage(dir.path(), &state, &part);

        let done = finish(&server, dir.path()).await;

        assert_eq!(done.resumed, 0, "{what}: nothing may be trusted");
        assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE), "{what}");
        assert_eq!(listing(dir.path()), [NAME], "{what}");
    }
}

#[tokio::test]
async fn resume_can_be_switched_off() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    pause_midway(&server, "range-hold", dir.path()).await;
    server.stop_throttling();
    let mut r = pieces(server.url(&format!("/range-hold/{SIZE}")), dir.path());
    r.resume = false;

    let done = complete(&r).await;

    assert_eq!(done.resumed, 0);
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
}

#[tokio::test]
async fn a_weak_etag_is_not_accepted_as_proof_for_resume() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancelToken::new();
    // Large enough that the download cannot finish before the pause: about 3 seconds at the server's limit.
    let r = pieces(server.url("/range-weak/4000000"), dir.path());

    // Several connections still work. But a pause cannot promise a safe
    // continuation without a strong validator, so it must keep nothing and say so.
    let outcome = download(&r, &cancel, &|event| {
        if matches!(event, Event::Progress { downloaded, .. } if downloaded > 200_000) {
            cancel.pause();
        }
    })
    .await;

    assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
    assert!(listing(dir.path()).is_empty(), "nothing that looks resumable may be left behind");
}

#[tokio::test]
async fn cancelling_a_segmented_download_keeps_nothing() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancelToken::new();
    let r = pieces(server.url(&format!("/range-hold/{SIZE}")), dir.path());

    let outcome = download(&r, &cancel, &|event| {
        if matches!(event, Event::Progress { downloaded, .. } if downloaded > 100_000) {
            cancel.cancel();
        }
    })
    .await;

    assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
    assert!(listing(dir.path()).is_empty());
}

#[tokio::test]
async fn a_failure_for_another_address_leaves_saved_progress_untouched() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    pause_midway(&server, "range-hold", dir.path()).await;

    let mut elsewhere = pieces(server.url(&format!("/missing/{SIZE}")), dir.path());
    elsewhere.target = quorix_core::Target::File(dir.path().join(NAME));
    let err = run(&elsewhere).await.unwrap_err();
    assert!(matches!(err, Error::Http { status: 404, .. }), "{err:?}");

    assert_eq!(listing(dir.path()), [format!("{NAME}.qrxpart"), format!("{NAME}.qrxpart.state")]);
    let done = finish(&server, dir.path()).await;
    assert!(done.resumed > 0);
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
}

#[tokio::test]
async fn bytes_changed_in_the_partial_file_are_found_and_downloaded_again() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (downloaded, _) = pause_midway(&server, "range-hold", dir.path()).await;

    // Another program damages the first block of the partial file while the download is paused.
    let part = dir.path().join(format!("{NAME}.qrxpart"));
    let mut bytes = std::fs::read(&part).unwrap();
    for b in &mut bytes[1000..1100] {
        *b ^= 0xff;
    }
    std::fs::write(&part, &bytes).unwrap();

    let checked = Mutex::new(None);
    server.stop_throttling();
    let done = download(&pieces(server.url(&format!("/range-hold/{SIZE}")), dir.path()), &CancelToken::new(), &|event| {
        if let Event::Rechecked { verified, discarded } = event {
            *checked.lock().unwrap() = Some((verified, discarded));
        }
    })
    .await
    .unwrap()
    .completed()
    .unwrap();

    let (verified, discarded) = checked.into_inner().unwrap().expect("saved data must be examined before reuse");
    assert!(discarded >= 64 * 1024, "the damaged block must be rejected: {discarded}");
    assert!(verified > 0 && verified + discarded == downloaded, "{verified} + {discarded} != {downloaded}");
    assert_eq!(done.resumed, verified);
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE), "the finished file must not contain the damaged bytes");
}

#[tokio::test]
async fn a_partial_file_that_fails_the_check_completely_means_a_clean_restart() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    pause_midway(&server, "range-hold", dir.path()).await;

    let part = dir.path().join(format!("{NAME}.qrxpart"));
    let len = std::fs::metadata(&part).unwrap().len() as usize;
    std::fs::write(&part, vec![0x42u8; len]).unwrap(); // right size, wrong content everywhere

    let done = finish(&server, dir.path()).await;

    assert_eq!(done.resumed, 0);
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
    assert_eq!(listing(dir.path()), [NAME]);
}

#[tokio::test]
async fn the_recheck_can_be_skipped_and_then_saved_data_is_reused_as_it_is() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (downloaded, _) = pause_midway(&server, "range-hold", dir.path()).await;

    server.stop_throttling();
    let mut r = pieces(server.url(&format!("/range-hold/{SIZE}")), dir.path());
    r.verify_resume = false;
    let done = complete(&r).await;

    assert_eq!(done.resumed, downloaded, "without the check every saved byte is taken on trust");
    assert_eq!(std::fs::read(&done.path).unwrap(), body(SIZE));
}
