// SPDX-License-Identifier: GPL-3.0-or-later

//! Segmented transfer: several connections fill one file.
//!
//! The file is cut into pieces. Workers take pieces from a shared queue, fetch
//! each with a range request and write the bytes at their final offsets. A slow
//! connection holds one piece at most, so the others keep the download moving.
//!
//! Two mechanisms adapt the transfer while it runs:
//!
//! - **Adaptive connection count.** The transfer starts with a few connections
//!   and measures the total speed. It doubles the count while each doubling
//!   gives a real gain, and goes back one step when it does not. A refusal from
//!   the server ends the growth for good.
//! - **Hedged requests.** When a worker has nothing left to take and another
//!   worker's piece is stalled or much slower than its own, it requests the
//!   rest of that piece as well. Whichever finishes first completes the piece.
//!
//! Invariants this module keeps:
//! - Bytes are accepted only from a `206` whose `Content-Range` matches the request.
//! - A worker never writes outside the piece it works on, whatever the server sends.
//! - A piece has one owner. Its bytes are written twice only by a hedge, and a
//!   hedge is allowed only when a strong `ETag` pins every request to one
//!   version of the file, so both writers hold identical bytes.
//! - `written` only ever lists bytes that have been handed to the file.
//! - A checkpoint only lists bytes that were synced to disk before it was saved.

use std::collections::{BTreeMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use quorix_http::{Client, Response, Url};
use quorix_integrity::blake3_hex;
use quorix_state::{block_range, Checkpoint, Ranges};
use quorix_storage::{PartFile, StorageError};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::cancel::StopKind;
use crate::engine::{Lifecycle, RetryPolicy, PROGRESS_INTERVAL, WRITE_BUFFER};
use crate::{CancelToken, Error, Event, TaskState};

/// Longest time received bytes may wait in memory before they are written.
const MAX_BUFFER_AGE: Duration = Duration::from_millis(500);
/// Connections an adaptive transfer starts with.
const INITIAL_CONNECTIONS: usize = 4;
/// Time over which the total speed is measured before the connection count is judged.
const MEASURE_WINDOW: Duration = Duration::from_millis(800);
/// A doubling of the connection count must raise the speed by this factor to be kept.
const WORTHWHILE_GAIN: f64 = 1.15;
/// A piece is hedged for slowness only when its owner is this many times slower than the idle worker was.
const SLOW_FACTOR: u64 = 4;
/// A piece younger than this has no meaningful speed yet.
const MIN_SAMPLE_MS: u64 = 500;
/// How often an idle worker looks for a piece to hedge.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// Failures in a row, with no bytes received, after which a network path is no longer used.
const PATH_STRIKES: u32 = 3;

/// One way out to the network: an HTTP client and the local address it sends from.
pub(crate) struct Route {
    pub client: Client,
    pub address: Option<IpAddr>,
}

/// Everything a segmented transfer needs.
pub(crate) struct Plan {
    /// Network paths to spread the connections over. Never empty.
    pub routes: Vec<Route>,
    pub url: Url,
    pub part: PartFile,
    pub total: u64,
    /// Strong `ETag`. Present means the download can be resumed and every range request is pinned to this version.
    pub strong_etag: Option<String>,
    /// Any `ETag`, strong or weak, used to notice a change between answers.
    pub validator: Option<String>,
    pub identity: String,
    /// Bytes already on disk from an earlier run.
    pub done: Ranges,
    /// The inspection answer, when its body can serve as the first piece: `(response, start, end)`.
    pub first: Option<(Response, u64, u64)>,
    pub piece_size: u64,
    /// Most connections to use.
    pub connections: usize,
    /// Start low and let measurements decide how many connections to use, up to `connections`.
    pub adaptive: bool,
    /// A piece with no progress for this long may be requested again by an idle worker.
    pub hedge_after: Duration,
    pub retry: RetryPolicy,
    pub checkpoint_interval: Duration,
    /// Size of the blocks that are hashed for the checkpoint.
    pub block_size: u64,
    /// Hashes of blocks already verified on disk, from an earlier run.
    pub hashes: BTreeMap<u64, String>,
}

impl Plan {
    /// Pieces still to fetch, excluding what the first answer already covers.
    fn pieces(&self) -> VecDeque<(u64, u64)> {
        let covered_by_first = self.first.as_ref().map_or(0, |(_, _, end)| *end);
        let mut pieces = VecDeque::new();
        for (start, end) in self.done.missing(self.total) {
            let mut at = start.max(covered_by_first);
            while at < end {
                let next = (at + self.piece_size).min(end);
                pieces.push_back((at, next));
                at = next;
            }
        }
        pieces
    }

    /// Connections the transfer starts with: never more than there are pieces.
    pub fn worker_count(&self) -> usize {
        let work = self.pieces().len() + usize::from(self.first.is_some());
        let wanted = if self.adaptive { self.connections.min(INITIAL_CONNECTIONS) } else { self.connections };
        wanted.min(work).max(1)
    }
}

/// How the transfer ended.
pub(crate) enum Ended {
    /// Every byte of the file is in the part file.
    Complete,
    /// Stopped on request; progress is saved.
    Paused { downloaded: u64 },
    /// Stopped by an error or a cancel. `keep` says whether the partial data may be reused later.
    Failed { error: Error, keep: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Run,
    Pause,
    Cancel,
    Fatal,
}

enum Note {
    Retrying,
    Flowing,
    /// The path with this index was taken out of use.
    PathDown(usize),
}

struct Path {
    client: Client,
    address: Option<IpAddr>,
    /// Failures in a row on this path with no bytes received.
    strikes: AtomicU32,
    down: AtomicBool,
}

/// One piece being fetched. Shared between its owner and, at most, one hedging worker.
struct Claim {
    start: u64,
    end: u64,
    /// How far the owner has written.
    position: AtomicU64,
    /// Set by whoever writes the last byte of the piece. The other side stops when it sees it.
    done: watch::Sender<bool>,
    hedged: AtomicBool,
    /// Milliseconds since the transfer started.
    began_ms: u64,
    progressed_ms: AtomicU64,
}

impl Claim {
    fn is_done(&self) -> bool {
        *self.done.borrow()
    }

    /// Average speed of the owner on this piece, in bytes per second.
    fn rate(&self, now_ms: u64) -> u64 {
        let bytes = self.position.load(Ordering::Relaxed).saturating_sub(self.start);
        bytes * 1000 / now_ms.saturating_sub(self.began_ms).max(1)
    }
}

struct Queue {
    pending: VecDeque<(u64, u64)>,
    /// Pieces being fetched right now.
    claims: Vec<Arc<Claim>>,
    /// Workers that may still take work. Changed only under the queue lock, so
    /// "the last worker left" and "work was handed back" cannot both happen.
    active: usize,
}

struct Shared {
    paths: Vec<Path>,
    url: Url,
    part: PartFile,
    total: u64,
    strong_etag: Option<String>,
    validator: Option<String>,
    retry: RetryPolicy,
    hedge_after: Duration,
    started: Instant,
    queue: Mutex<Queue>,
    /// Connections the controller currently permits. A worker above this number leaves at its next piece boundary.
    allowed: AtomicUsize,
    /// Set when the server refused a connection. The controller never grows again after that.
    refused: AtomicBool,
    written: Mutex<Ranges>,
    block_size: u64,
    /// BLAKE3 hash of every block that is complete and was read back from the file.
    hashes: Mutex<BTreeMap<u64, String>>,
    downloaded: AtomicU64,
    /// Bytes taken from the network by piece owners, counted as they arrive.
    /// The controller measures speed with this: bytes reach the disk in large
    /// steps, which is too coarse to judge a short window.
    received: AtomicU64,
    /// Failures in a row with no bytes received in between, across all workers.
    failures: AtomicU32,
    fatal: Mutex<Option<Error>>,
    stop: watch::Sender<Stop>,
    notes: mpsc::UnboundedSender<Note>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock means another worker panicked; the data is plain values and still usable.
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum Pumped {
    Done,
    Short,
    Stopped,
    /// The other worker on this piece finished it.
    Superseded,
}

enum Next {
    Retry,
    Leave,
}

enum Work {
    /// A piece from the queue. The worker owns it.
    Piece(Arc<Claim>),
    /// Another worker's piece, to be fetched again from the given offset.
    Hedge(Arc<Claim>, u64),
    /// Nothing to take now, but a piece in flight may need a hedge later.
    Wait,
    /// Nothing left for this worker.
    Leave,
}

impl Shared {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// The path to use for worker number `index`: its own when it is up, otherwise the next one that is.
    fn usable_path(&self, index: usize) -> usize {
        let count = self.paths.len();
        (0..count).map(|step| (index + step) % count).find(|&i| !self.paths[i].down.load(Ordering::Relaxed)).unwrap_or(index % count)
    }

    /// Counts a failure of `path` itself: no connection, a timeout, or a
    /// connection that closed before any byte arrived. Returns another path to
    /// move to when one is up. `None` means this is the only path left, and
    /// the failure is handled like any other.
    fn path_failed(&self, path: usize) -> Option<usize> {
        let count = self.paths.len();
        let other = (1..count).map(|step| (path + step) % count).find(|&i| !self.paths[i].down.load(Ordering::Relaxed))?;
        let strikes = self.paths[path].strikes.fetch_add(1, Ordering::Relaxed) + 1;
        if strikes >= PATH_STRIKES && !self.paths[path].down.swap(true, Ordering::Relaxed) {
            let _ = self.notes.send(Note::PathDown(path));
        }
        Some(other)
    }

    /// Records the first fatal error and tells every worker to stop.
    fn fail(&self, error: Error) {
        lock(&self.fatal).get_or_insert(error);
        self.stop.send_if_modified(|stop| {
            let running = *stop == Stop::Run;
            if running {
                *stop = Stop::Fatal;
            }
            running
        });
    }

    fn claim(&self, queue: &mut Queue, start: u64, end: u64) -> Arc<Claim> {
        let now = self.now_ms();
        let claim = Arc::new(Claim {
            start,
            end,
            position: AtomicU64::new(start),
            done: watch::channel(false).0,
            hedged: AtomicBool::new(false),
            began_ms: now,
            progressed_ms: AtomicU64::new(now),
        });
        queue.claims.push(Arc::clone(&claim));
        claim
    }

    fn release(&self, claim: &Arc<Claim>) {
        lock(&self.queue).claims.retain(|c| !Arc::ptr_eq(c, claim));
    }

    /// Decides what a worker does next. `my_rate` is the speed the worker
    /// reached on its last piece, in bytes per second, or 0 when unknown.
    fn take(&self, my_rate: u64) -> Work {
        let mut queue = lock(&self.queue);
        if queue.active > self.allowed.load(Ordering::Relaxed).max(1) {
            queue.active -= 1;
            return Work::Leave;
        }
        if let Some((start, end)) = queue.pending.pop_front() {
            return Work::Piece(self.claim(&mut queue, start, end));
        }

        // Hedging writes the same bytes twice. That is only safe when every
        // request is pinned to one version of the file by a strong ETag.
        if self.strong_etag.is_none() {
            queue.active -= 1;
            return Work::Leave;
        }
        let now = self.now_ms();
        let stall_ms = self.hedge_after.as_millis() as u64;
        let mut open_claims = false;
        let mut best: Option<(&Arc<Claim>, u64)> = None;
        for claim in &queue.claims {
            if claim.is_done() || claim.hedged.load(Ordering::Relaxed) {
                continue;
            }
            open_claims = true;
            let remaining = claim.end - claim.position.load(Ordering::Relaxed);
            let stalled = now.saturating_sub(claim.progressed_ms.load(Ordering::Relaxed)) >= stall_ms;
            let slow = my_rate > 0 && now.saturating_sub(claim.began_ms) >= MIN_SAMPLE_MS && claim.rate(now) * SLOW_FACTOR < my_rate;
            if (stalled || slow) && remaining > 0 && best.is_none_or(|(_, most)| remaining > most) {
                best = Some((claim, remaining));
            }
        }
        match best {
            Some((claim, _)) => {
                claim.hedged.store(true, Ordering::Relaxed);
                Work::Hedge(Arc::clone(claim), claim.position.load(Ordering::Relaxed))
            }
            None if open_claims => Work::Wait,
            None => {
                queue.active -= 1;
                Work::Leave
            }
        }
    }

    /// Requests bytes `start..end` and checks that the answer is exactly that.
    /// Returns the response and the end of the range it actually carries.
    async fn open(&self, path: usize, start: u64, end: u64) -> Result<(Response, u64), Error> {
        let response = self.paths[path].client.get_range(&self.url, start, end - 1, self.strong_etag.as_deref()).await?;
        match response.status() {
            206 => {
                let range = response
                    .content_range()
                    .ok_or_else(|| Error::Protocol("a partial answer came without a valid Content-Range".into()))?;
                if range.start != start || range.end >= end || range.total != Some(self.total) {
                    return Err(Error::Protocol(format!(
                        "bytes {start}-{} of {} were requested but the answer describes bytes {}-{} of {}",
                        end - 1,
                        self.total,
                        range.start,
                        range.end,
                        range.total.map_or("unknown".to_string(), |t| t.to_string())
                    )));
                }
                if response.content_encoding().is_some() {
                    return Err(Error::Protocol("a range was sent with a content encoding although none was requested".into()));
                }
                if let (Some(known), Some(now)) = (&self.validator, response.etag()) {
                    if known != now {
                        return Err(Error::ResourceChanged);
                    }
                }
                Ok((response, range.end + 1))
            }
            // With If-Range, a full answer is the server saying "that version is gone".
            200 if self.strong_etag.is_some() => Err(Error::ResourceChanged),
            200 => Err(Error::Protocol("the server stopped honouring range requests".into())),
            status => Err(Error::Http { status, retry_after: response.retry_after_secs() }),
        }
    }

    /// Hands buffered bytes to the file and records them as written.
    /// Only bytes that were not on disk before count as progress, so a hedge never inflates the total.
    async fn flush(&self, buffer: &mut Vec<u8>, position: &mut u64) -> Result<(), Error> {
        if buffer.is_empty() {
            return Ok(());
        }
        let length = buffer.len() as u64;
        *buffer = self.part.write_at(*position, std::mem::take(buffer)).await?;
        let added = lock(&self.written).insert(*position, *position + length);
        *position += length;
        self.downloaded.fetch_add(added, Ordering::Relaxed);
        Ok(())
    }

    /// Publishes the owner's progress on a piece, so idle workers can judge whether it needs a hedge.
    fn report(&self, claim: &Claim, owner: bool, position: u64) {
        if owner {
            claim.position.store(position, Ordering::Relaxed);
            claim.progressed_ms.store(self.now_ms(), Ordering::Relaxed);
        }
    }

    /// Reads a response body into the file, from `position` up to `end` and never beyond.
    /// `owner` is true for the worker that owns `claim`; only the owner reports its position.
    async fn pump(
        &self,
        response: &mut Response,
        position: &mut u64,
        end: u64,
        claim: &Claim,
        owner: bool,
        stop: &mut watch::Receiver<Stop>,
    ) -> Result<Pumped, Error> {
        let mut finished = claim.done.subscribe();
        let mut buffer = Vec::with_capacity(WRITE_BUFFER.min((end - *position) as usize));
        let mut last_flush = Instant::now();
        loop {
            if *finished.borrow() {
                return Ok(Pumped::Superseded);
            }
            let chunk = tokio::select! {
                biased;
                _ = stop.changed() => {
                    // On pause keep what already arrived. On cancel or failure it is about to be judged elsewhere.
                    if *stop.borrow() == Stop::Pause {
                        self.flush(&mut buffer, position).await?;
                    }
                    return Ok(Pumped::Stopped);
                }
                _ = finished.changed() => return Ok(Pumped::Superseded),
                // A connection that goes silent must not hold received bytes in memory.
                _ = tokio::time::sleep(MAX_BUFFER_AGE), if !buffer.is_empty() => {
                    self.flush(&mut buffer, position).await?;
                    last_flush = Instant::now();
                    self.report(claim, owner, *position);
                    continue;
                }
                chunk = response.chunk() => chunk,
            };
            match chunk {
                Err(e) => {
                    // Bytes received before the break came with a validated header. They are good.
                    self.flush(&mut buffer, position).await?;
                    return Err(e.into());
                }
                Ok(None) => {
                    self.flush(&mut buffer, position).await?;
                    return Ok(if *position == end { Pumped::Done } else { Pumped::Short });
                }
                Ok(Some(bytes)) => {
                    let room = (end - *position - buffer.len() as u64) as usize;
                    let taken = bytes.len().min(room);
                    buffer.extend_from_slice(&bytes[..taken]);
                    if owner {
                        self.received.fetch_add(taken as u64, Ordering::Relaxed);
                    }
                    let piece_full = *position + buffer.len() as u64 == end;
                    // On a slow link the buffer fills slowly. Flushing by age bounds what a crash can lose.
                    if buffer.len() >= WRITE_BUFFER || piece_full || last_flush.elapsed() >= MAX_BUFFER_AGE {
                        self.flush(&mut buffer, position).await?;
                        last_flush = Instant::now();
                        self.report(claim, owner, *position);
                    }
                    if piece_full {
                        return Ok(Pumped::Done);
                    }
                }
            }
        }
    }

    /// Fetches the rest of another worker's piece. One attempt: a hedge that
    /// fails is simply dropped, because the owner is still working on the piece.
    async fn hedge(&self, claim: &Claim, from: u64, path: usize, stop: &mut watch::Receiver<Stop>) {
        let mut position = from;
        let mut finished = claim.done.subscribe();
        while position < claim.end && !claim.is_done() {
            let opened = tokio::select! {
                biased;
                _ = stop.changed() => return,
                _ = finished.changed() => return,
                opened = self.open(path, position, claim.end) => opened,
            };
            let outcome = match opened {
                Ok((mut response, range_end)) => self.pump(&mut response, &mut position, range_end, claim, false, stop).await,
                Err(error) => Err(error),
            };
            match outcome {
                Ok(Pumped::Done) => {}
                Ok(Pumped::Stopped | Pumped::Superseded) => return,
                // A changed file or a storage failure is real for the whole download, not only for the hedge.
                Err(error @ (Error::ResourceChanged | Error::DiskFull | Error::Io(_))) => return self.fail(error),
                Ok(Pumped::Short) | Err(_) => {
                    claim.hedged.store(false, Ordering::Relaxed); // let another idle worker try later
                    return;
                }
            }
        }
        if position == claim.end {
            claim.done.send_replace(true);
        }
    }

    /// Decides what a worker does after a failed attempt on the rest of its piece.
    async fn after_failure(
        &self,
        error: Error,
        progressed: bool,
        claim: &Arc<Claim>,
        position: u64,
        stop: &mut watch::Receiver<Stop>,
    ) -> Next {
        // A refusal while other connections are working usually means "too many
        // connections". Give the work back and continue with fewer (SOP 5.5).
        if matches!(error, Error::Http { status: 403 | 429 | 503, .. }) {
            let mut queue = lock(&self.queue);
            if queue.active > 1 {
                self.refused.store(true, Ordering::Relaxed);
                queue.claims.retain(|c| !Arc::ptr_eq(c, claim));
                if !claim.is_done() {
                    queue.pending.push_front((position, claim.end));
                }
                queue.active -= 1;
                return Next::Leave;
            }
        }

        let attempt = if progressed { 1 } else { self.failures.fetch_add(1, Ordering::Relaxed) + 1 };
        let Some(delay) = self.retry.delay(attempt, &error) else {
            self.fail(error);
            return Next::Leave;
        };
        let _ = self.notes.send(Note::Retrying);
        tokio::select! {
            biased;
            _ = stop.changed() => Next::Leave,
            _ = tokio::time::sleep(delay) => Next::Retry,
        }
    }

    /// Hashes every block that is completely inside `on_disk` and has no hash yet.
    ///
    /// The block is read back from the file, so the hash describes what the
    /// file holds, not what the engine believes it wrote. A later run compares
    /// the file with these hashes before it trusts a single saved byte.
    async fn hash_new_blocks(&self, on_disk: &Ranges) -> Result<(), Error> {
        let mut todo = Vec::new();
        {
            let known = lock(&self.hashes);
            for &(start, end) in on_disk.spans() {
                let mut index = start.div_ceil(self.block_size);
                loop {
                    let (block_start, block_end) = block_range(index, self.block_size, self.total);
                    if block_start >= block_end || block_end > end {
                        break;
                    }
                    if !known.contains_key(&index) {
                        todo.push((index, block_start, block_end));
                    }
                    index += 1;
                }
            }
        }
        for (index, start, end) in todo {
            let data = self.part.read_at(start, (end - start) as usize).await?;
            let hash = tokio::task::spawn_blocking(move || blake3_hex(&data)).await.map_err(|e| Error::Internal(e.to_string()))?;
            lock(&self.hashes).insert(index, hash);
        }
        Ok(())
    }

    /// Saves confirmed progress. The snapshot is taken before the sync, so
    /// every byte it lists was written before the sync started and is durable
    /// once the sync returns. Only then is the checkpoint replaced.
    async fn checkpoint(&self, identity: &str, etag: &str) -> Result<(), Error> {
        let snapshot = lock(&self.written).clone();
        self.part.sync_data().await?;
        self.hash_new_blocks(&snapshot).await?;
        let hashes = lock(&self.hashes).clone();
        let checkpoint = Checkpoint::new(identity.to_string(), self.total, etag.to_string(), &snapshot, self.block_size, &hashes);
        let path = self.part.state_path().to_path_buf();
        tokio::task::spawn_blocking(move || checkpoint.save(&path))
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .map_err(|e| Error::from(StorageError::from(e)))
    }
}

/// Errors that say the way to the server failed, not that the server gave a bad answer.
fn is_path_failure(error: &Error) -> bool {
    matches!(error, Error::Connect(_) | Error::Timeout | Error::ConnectionClosed)
}

async fn worker(shared: Arc<Shared>, first: Option<(Response, Arc<Claim>)>, index: usize) {
    // Each worker has a home path. After a failure it finishes the current piece
    // on another path, then tries its home again. So a path that keeps failing
    // collects strikes and is taken out of use, while one bad moment costs nothing.
    let home = index % shared.paths.len();
    let mut stop = shared.stop.subscribe();
    let mut carried = first;
    let mut my_rate = 0u64;
    loop {
        let (mut ready, claim) = match carried.take() {
            Some((response, claim)) => (Some(response), claim),
            None => match shared.take(my_rate) {
                Work::Piece(claim) => (None, claim),
                Work::Hedge(claim, from) => {
                    shared.hedge(&claim, from, shared.usable_path(home), &mut stop).await;
                    continue;
                }
                Work::Wait => {
                    tokio::select! {
                        biased;
                        _ = stop.changed() => return,
                        _ = tokio::time::sleep(IDLE_POLL) => continue,
                    }
                }
                Work::Leave => return,
            },
        };

        let mut path = shared.usable_path(home);
        let began = Instant::now();
        let mut position = claim.start;
        let mut finished = claim.done.subscribe();
        while position < claim.end && !claim.is_done() {
            if *stop.borrow() != Stop::Run {
                return;
            }
            let opened = match ready.take() {
                Some(response) => Ok((response, claim.end)),
                None => tokio::select! {
                    biased;
                    _ = stop.changed() => return,
                    _ = finished.changed() => break,
                    opened = shared.open(path, position, claim.end) => opened,
                },
            };
            let failure = match opened {
                Err(error) => (error, false),
                Ok((mut response, range_end)) => {
                    let _ = shared.notes.send(Note::Flowing);
                    let before = position;
                    let pumped = shared.pump(&mut response, &mut position, range_end, &claim, true, &mut stop).await;
                    let progressed = position > before;
                    if progressed {
                        shared.failures.store(0, Ordering::Relaxed);
                        shared.paths[path].strikes.store(0, Ordering::Relaxed);
                    }
                    match pumped {
                        Ok(Pumped::Done) => continue,
                        Ok(Pumped::Superseded) => break,
                        Ok(Pumped::Stopped) => return,
                        Ok(Pumped::Short) => (Error::ConnectionClosed, progressed),
                        Err(error) => (error, progressed),
                    }
                }
            };
            // A failed path is not a failed download while another path works: move there and try at once.
            if !failure.1 && is_path_failure(&failure.0) {
                if let Some(other) = shared.path_failed(path) {
                    path = other;
                    continue;
                }
            }
            match shared.after_failure(failure.0, failure.1, &claim, position, &mut stop).await {
                Next::Retry => {}
                Next::Leave => return,
            }
        }

        if position == claim.end {
            claim.done.send_replace(true);
        }
        shared.release(&claim);
        let bytes = position - claim.start;
        if bytes > 0 {
            my_rate = bytes * 1000 / (began.elapsed().as_millis() as u64).max(1);
        }
    }
}

/// Errors after which the partial data must not be reused.
fn poisons_partial_data(error: &Error) -> bool {
    matches!(error, Error::ResourceChanged | Error::Protocol(_) | Error::LengthMismatch { .. } | Error::Internal(_) | Error::Cancelled)
}

/// Finds the connection count worth using by measuring, not by assuming.
struct Controller {
    enabled: bool,
    limit: usize,
    /// Connection count before the last doubling, and the speed measured there.
    previous: Option<(usize, f64)>,
    window_started: Instant,
    window_bytes: u64,
    /// The first window of the transfer and the first after each change are not judged:
    /// connections are still starting, so the speed reads low and the next window would look like a gain.
    settling: bool,
}

enum Verdict {
    Keep,
    Grow(usize),
    Shrink(usize),
}

impl Controller {
    fn new(enabled: bool, limit: usize, bytes: u64) -> Self {
        Self { enabled, limit, previous: None, window_started: Instant::now(), window_bytes: bytes, settling: true }
    }

    /// Call often. Returns a verdict once per measurement window.
    fn judge(&mut self, current: usize, bytes: u64, refused: bool) -> Verdict {
        if !self.enabled {
            return Verdict::Keep;
        }
        if refused {
            // The server drew the line. Respect it for the rest of the download.
            self.enabled = false;
            return Verdict::Keep;
        }
        let elapsed = self.window_started.elapsed();
        if elapsed < MEASURE_WINDOW {
            return Verdict::Keep;
        }
        let rate = (bytes - self.window_bytes) as f64 / elapsed.as_secs_f64();
        self.window_started = Instant::now();
        self.window_bytes = bytes;
        if std::mem::take(&mut self.settling) {
            return Verdict::Keep;
        }

        match self.previous {
            Some((before, rate_before)) if rate < rate_before * WORTHWHILE_GAIN => {
                // Twice the connections for no real gain: the limit is the network or the disk, not the server.
                self.enabled = false;
                Verdict::Shrink(before)
            }
            _ => {
                let next = (current * 2).min(self.limit);
                if next == current {
                    self.enabled = false;
                    return Verdict::Keep;
                }
                self.previous = Some((current, rate));
                self.settling = true;
                Verdict::Grow(next)
            }
        }
    }
}

/// Runs the transfer until the file is complete, the caller stops it, or it fails.
pub(crate) async fn run(mut plan: Plan, cancel: &CancelToken, life: &mut Lifecycle<'_>) -> Ended {
    let workers = plan.worker_count();
    let pending = plan.pieces();
    let first = plan.first.take();
    let resumed = plan.done.covered();
    let (identity, etag, interval, total) = (plan.identity, plan.strong_etag.clone(), plan.checkpoint_interval, plan.total);
    let mut controller = Controller::new(plan.adaptive, plan.connections, 0);

    let (stop_tx, _) = watch::channel(Stop::Run);
    let (note_tx, mut notes) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        paths: plan
            .routes
            .into_iter()
            .map(|route| Path { client: route.client, address: route.address, strikes: AtomicU32::new(0), down: AtomicBool::new(false) })
            .collect(),
        url: plan.url,
        part: plan.part,
        total,
        strong_etag: plan.strong_etag,
        validator: plan.validator,
        retry: plan.retry,
        hedge_after: plan.hedge_after,
        started: Instant::now(),
        queue: Mutex::new(Queue { pending, claims: Vec::new(), active: workers }),
        allowed: AtomicUsize::new(workers),
        refused: AtomicBool::new(false),
        written: Mutex::new(plan.done),
        block_size: plan.block_size,
        hashes: Mutex::new(plan.hashes),
        downloaded: AtomicU64::new(resumed),
        received: AtomicU64::new(0),
        failures: AtomicU32::new(0),
        fatal: Mutex::new(None),
        stop: stop_tx,
        notes: note_tx,
    });

    let mut tasks = JoinSet::new();
    let mut first = first.map(|(response, start, end)| {
        let claim = shared.claim(&mut lock(&shared.queue), start, end);
        (response, claim)
    });
    let mut spawned = 0;
    for _ in 0..workers {
        tasks.spawn(worker(Arc::clone(&shared), first.take(), spawned));
        spawned += 1;
    }

    let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_checkpoint = Instant::now();
    let stopped = cancel.stopped();
    tokio::pin!(stopped);
    let mut asked = false;

    loop {
        tokio::select! {
            kind = &mut stopped, if !asked => {
                asked = true;
                let wanted = if kind == StopKind::Pause { Stop::Pause } else { Stop::Cancel };
                shared.stop.send_if_modified(|stop| {
                    let running = *stop == Stop::Run;
                    if running {
                        *stop = wanted;
                    }
                    running
                });
            }
            joined = tasks.join_next() => match joined {
                None => break,
                Some(Ok(())) => {}
                Some(Err(e)) => shared.fail(Error::Internal(format!("a transfer task stopped unexpectedly: {e}"))),
            },
            Some(note) = notes.recv() => match (note, life.state()) {
                (Note::Retrying, TaskState::Downloading) => life.enter(TaskState::Retrying),
                (Note::Flowing, TaskState::Retrying) => life.enter(TaskState::Downloading),
                (Note::PathDown(index), _) => {
                    if let Some(address) = shared.paths[index].address {
                        (life.sink)(Event::PathDown(address));
                    }
                }
                _ => {}
            },
            _ = ticker.tick() => {
                let bytes = shared.downloaded.load(Ordering::Relaxed);
                (life.sink)(Event::Progress { downloaded: bytes, total: Some(total) });

                let current = shared.allowed.load(Ordering::Relaxed);
                let received = shared.received.load(Ordering::Relaxed);
                match controller.judge(current, received, shared.refused.load(Ordering::Relaxed)) {
                    Verdict::Keep => {}
                    Verdict::Shrink(to) => {
                        shared.allowed.store(to, Ordering::Relaxed);
                        (life.sink)(Event::Connections(to));
                    }
                    Verdict::Grow(to) => {
                        // Only start workers that have a piece waiting for them.
                        let extra = {
                            let mut queue = lock(&shared.queue);
                            let extra = (to - current).min(queue.pending.len());
                            queue.active += extra;
                            extra
                        };
                        if extra > 0 && *shared.stop.borrow() == Stop::Run {
                            shared.allowed.store(current + extra, Ordering::Relaxed);
                            for _ in 0..extra {
                                tasks.spawn(worker(Arc::clone(&shared), None, spawned));
                                spawned += 1;
                            }
                            (life.sink)(Event::Connections(current + extra));
                        } else {
                            lock(&shared.queue).active -= extra;
                        }
                    }
                }

                if let Some(etag) = etag.as_deref().filter(|_| last_checkpoint.elapsed() >= interval) {
                    last_checkpoint = Instant::now();
                    if let Err(e) = shared.checkpoint(&identity, etag).await {
                        shared.fail(e);
                    }
                }
            }
        }
    }

    let downloaded = lock(&shared.written).covered();
    let complete = lock(&shared.written).is_complete(total);
    (life.sink)(Event::Progress { downloaded, total: Some(total) });
    let stop = *shared.stop.borrow();
    let fatal = lock(&shared.fatal).take();

    if cancel.is_cancelled() || stop == Stop::Cancel {
        return Ended::Failed { error: Error::Cancelled, keep: false };
    }
    if let Some(error) = fatal {
        let mut keep = etag.is_some() && downloaded > 0 && !poisons_partial_data(&error);
        if keep {
            // Save what arrived so the next run continues from here. If even that fails, reuse nothing.
            keep = shared.checkpoint(&identity, etag.as_deref().unwrap_or_default()).await.is_ok();
        }
        return Ended::Failed { error, keep };
    }
    if complete {
        if life.state() == TaskState::Retrying {
            life.enter(TaskState::Downloading);
        }
        return Ended::Complete;
    }
    if stop == Stop::Pause {
        // Without a strong ETag a later run could not prove the file is unchanged, so nothing is kept.
        let Some(etag) = etag.as_deref() else { return Ended::Failed { error: Error::Cancelled, keep: false } };
        life.enter(TaskState::Pausing);
        return match shared.checkpoint(&identity, etag).await {
            Ok(()) => {
                life.enter(TaskState::Paused);
                Ended::Paused { downloaded }
            }
            Err(error) => Ended::Failed { error, keep: false },
        };
    }
    Ended::Failed { error: Error::Internal(format!("the transfer ended with {downloaded} of {total} bytes and no error")), keep: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds the controller one finished measurement window with the given speed.
    fn window(controller: &mut Controller, current: usize, bytes_per_second: u64, total: &mut u64) -> Verdict {
        controller.window_started = Instant::now() - MEASURE_WINDOW;
        *total += bytes_per_second * MEASURE_WINDOW.as_millis() as u64 / 1000;
        controller.judge(current, *total, false)
    }

    #[test]
    fn the_controller_grows_while_doubling_pays_and_steps_back_when_it_does_not() {
        let mut c = Controller::new(true, 16, 0);
        let mut total = 0;
        assert!(matches!(window(&mut c, 4, 300_000, &mut total), Verdict::Keep), "start-up window is not judged, however slow it looks");
        assert!(matches!(window(&mut c, 4, 1_000_000, &mut total), Verdict::Grow(8)), "first measurement: try more");
        assert!(matches!(window(&mut c, 8, 2_000_000, &mut total), Verdict::Keep), "warm-up window is not judged");
        assert!(matches!(window(&mut c, 8, 2_000_000, &mut total), Verdict::Grow(16)), "twice the speed: keep growing");
        assert!(matches!(window(&mut c, 16, 2_100_000, &mut total), Verdict::Keep), "warm-up window is not judged");
        assert!(matches!(window(&mut c, 16, 2_100_000, &mut total), Verdict::Shrink(8)), "5% more for twice the connections: go back");
        assert!(matches!(window(&mut c, 8, 9_000_000, &mut total), Verdict::Keep), "settled for the rest of the download");
    }

    #[test]
    fn the_controller_stops_at_the_limit_and_after_a_refusal() {
        let mut c = Controller::new(true, 4, 0);
        let mut total = 0;
        assert!(matches!(window(&mut c, 4, 1_000_000, &mut total), Verdict::Keep), "start-up window");
        assert!(matches!(window(&mut c, 4, 1_000_000, &mut total), Verdict::Keep), "already at the limit");

        let mut c = Controller::new(true, 16, 0);
        c.window_started = Instant::now() - MEASURE_WINDOW;
        assert!(matches!(c.judge(4, 1_000_000, true), Verdict::Keep), "the server refused a connection");
        assert!(matches!(window(&mut c, 4, 5_000_000, &mut total), Verdict::Keep), "and growth never resumes");
    }

    #[test]
    fn a_fixed_connection_count_is_never_changed() {
        let mut c = Controller::new(false, 16, 0);
        let mut total = 0;
        assert!(matches!(window(&mut c, 4, 1_000_000, &mut total), Verdict::Keep));
    }
}
