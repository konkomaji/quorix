// SPDX-License-Identifier: GPL-3.0-or-later

//! Segmented transfer: several connections fill one file.
//!
//! The file is cut into pieces. Workers take pieces from a shared queue, fetch
//! each with a range request and write the bytes at their final offsets. A slow
//! connection holds one piece at most, so the others keep the download moving.
//!
//! Invariants this module keeps:
//! - A piece belongs to one worker at a time, so no two workers write the same bytes.
//! - Bytes are accepted only from a `206` whose `Content-Range` matches the request.
//! - A worker never writes outside the piece it holds, whatever the server sends.
//! - `written` only ever lists bytes that have been handed to the file.
//! - A checkpoint only lists bytes that were synced to disk before it was saved.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use quorix_http::{Client, Response, Url};
use quorix_state::{Checkpoint, Ranges};
use quorix_storage::{PartFile, StorageError};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::cancel::StopKind;
use crate::engine::{Lifecycle, RetryPolicy, PROGRESS_INTERVAL, WRITE_BUFFER};
use crate::{CancelToken, Error, Event, TaskState};

/// Longest time received bytes may wait in memory before they are written.
const MAX_BUFFER_AGE: Duration = Duration::from_millis(500);

/// Everything a segmented transfer needs.
pub(crate) struct Plan {
    pub client: Client,
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
    pub connections: usize,
    pub retry: RetryPolicy,
    pub checkpoint_interval: Duration,
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

    /// Connections that will really be used: never more than there are pieces.
    pub fn worker_count(&self) -> usize {
        let work = self.pieces().len() + usize::from(self.first.is_some());
        self.connections.min(work).max(1)
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
}

struct Queue {
    pending: VecDeque<(u64, u64)>,
    /// Workers that may still take work. Changed only under the queue lock, so
    /// "the last worker left" and "work was handed back" cannot both happen.
    active: usize,
}

struct Shared {
    client: Client,
    url: Url,
    part: PartFile,
    total: u64,
    strong_etag: Option<String>,
    validator: Option<String>,
    retry: RetryPolicy,
    queue: Mutex<Queue>,
    written: Mutex<Ranges>,
    downloaded: AtomicU64,
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
}

enum Next {
    Retry,
    Leave,
}

impl Shared {
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

    /// Requests bytes `start..end` and checks that the answer is exactly that.
    /// Returns the response and the end of the range it actually carries.
    async fn open(&self, start: u64, end: u64) -> Result<(Response, u64), Error> {
        let response = self.client.get_range(&self.url, start, end - 1, self.strong_etag.as_deref()).await?;
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
    async fn flush(&self, buffer: &mut Vec<u8>, position: &mut u64) -> Result<(), Error> {
        if buffer.is_empty() {
            return Ok(());
        }
        let length = buffer.len() as u64;
        *buffer = self.part.write_at(*position, std::mem::take(buffer)).await?;
        lock(&self.written).insert(*position, *position + length);
        *position += length;
        self.downloaded.fetch_add(length, Ordering::Relaxed);
        Ok(())
    }

    /// Reads a response body into the file, from `position` up to `end` and never beyond.
    async fn pump(&self, response: &mut Response, position: &mut u64, end: u64, stop: &mut watch::Receiver<Stop>) -> Result<Pumped, Error> {
        let mut buffer = Vec::with_capacity(WRITE_BUFFER.min((end - *position) as usize));
        let mut last_flush = Instant::now();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = stop.changed() => {
                    // On pause keep what already arrived. On cancel or failure it is about to be judged elsewhere.
                    if *stop.borrow() == Stop::Pause {
                        self.flush(&mut buffer, position).await?;
                    }
                    return Ok(Pumped::Stopped);
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
                    buffer.extend_from_slice(&bytes[..bytes.len().min(room)]);
                    let piece_full = *position + buffer.len() as u64 == end;
                    // On a slow link the buffer fills slowly. Flushing by age bounds what a crash can lose.
                    if buffer.len() >= WRITE_BUFFER || piece_full || last_flush.elapsed() >= MAX_BUFFER_AGE {
                        self.flush(&mut buffer, position).await?;
                        last_flush = Instant::now();
                    }
                    if piece_full {
                        return Ok(Pumped::Done);
                    }
                }
            }
        }
    }

    /// Decides what a worker does after a failed attempt on `remaining`.
    async fn after_failure(&self, error: Error, progressed: bool, remaining: (u64, u64), stop: &mut watch::Receiver<Stop>) -> Next {
        // A refusal while other connections are working usually means "too many
        // connections". Give the work back and continue with fewer (SOP 5.5).
        if matches!(error, Error::Http { status: 403 | 429 | 503, .. }) {
            let mut queue = lock(&self.queue);
            if queue.active > 1 {
                queue.pending.push_front(remaining);
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

    /// Saves confirmed progress. The snapshot is taken before the sync, so
    /// every byte it lists was written before the sync started and is durable
    /// once the sync returns. Only then is the checkpoint replaced.
    async fn checkpoint(&self, identity: &str, etag: &str) -> Result<(), Error> {
        let snapshot = lock(&self.written).clone();
        self.part.sync_data().await?;
        let checkpoint = Checkpoint::new(identity.to_string(), self.total, etag.to_string(), &snapshot);
        let path = self.part.state_path().to_path_buf();
        tokio::task::spawn_blocking(move || checkpoint.save(&path))
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .map_err(|e| Error::from(StorageError::from(e)))
    }
}

async fn worker(shared: Arc<Shared>, first: Option<(Response, u64, u64)>) {
    let mut stop = shared.stop.subscribe();
    let mut carried = first;
    loop {
        let (mut ready, mut position, end) = match carried.take() {
            Some((response, start, end)) => (Some(response), start, end),
            None => {
                let mut queue = lock(&shared.queue);
                match queue.pending.pop_front() {
                    Some((start, end)) => (None, start, end),
                    None => {
                        queue.active -= 1;
                        return;
                    }
                }
            }
        };

        while position < end {
            if *stop.borrow() != Stop::Run {
                return;
            }
            let opened = match ready.take() {
                Some(response) => Ok((response, end)),
                None => tokio::select! {
                    biased;
                    _ = stop.changed() => return,
                    opened = shared.open(position, end) => opened,
                },
            };
            let failure = match opened {
                Err(error) => (error, false),
                Ok((mut response, range_end)) => {
                    let _ = shared.notes.send(Note::Flowing);
                    let before = position;
                    let pumped = shared.pump(&mut response, &mut position, range_end, &mut stop).await;
                    let progressed = position > before;
                    if progressed {
                        shared.failures.store(0, Ordering::Relaxed);
                    }
                    match pumped {
                        Ok(Pumped::Done) => continue,
                        Ok(Pumped::Stopped) => return,
                        Ok(Pumped::Short) => (Error::ConnectionClosed, progressed),
                        Err(error) => (error, progressed),
                    }
                }
            };
            match shared.after_failure(failure.0, failure.1, (position, end), &mut stop).await {
                Next::Retry => {}
                Next::Leave => return,
            }
        }
    }
}

/// Errors after which the partial data must not be reused.
fn poisons_partial_data(error: &Error) -> bool {
    matches!(error, Error::ResourceChanged | Error::Protocol(_) | Error::LengthMismatch { .. } | Error::Internal(_) | Error::Cancelled)
}

/// Runs the transfer until the file is complete, the caller stops it, or it fails.
pub(crate) async fn run(mut plan: Plan, cancel: &CancelToken, life: &mut Lifecycle<'_>) -> Ended {
    let workers = plan.worker_count();
    let pending = plan.pieces();
    let first = plan.first.take();
    let resumed = plan.done.covered();
    let (identity, etag, interval, total) = (plan.identity, plan.strong_etag.clone(), plan.checkpoint_interval, plan.total);

    let (stop_tx, _) = watch::channel(Stop::Run);
    let (note_tx, mut notes) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        client: plan.client,
        url: plan.url,
        part: plan.part,
        total,
        strong_etag: plan.strong_etag,
        validator: plan.validator,
        retry: plan.retry,
        queue: Mutex::new(Queue { pending, active: workers }),
        written: Mutex::new(plan.done),
        downloaded: AtomicU64::new(resumed),
        failures: AtomicU32::new(0),
        fatal: Mutex::new(None),
        stop: stop_tx,
        notes: note_tx,
    });

    let mut tasks = JoinSet::new();
    let mut first = first;
    for _ in 0..workers {
        tasks.spawn(worker(Arc::clone(&shared), first.take()));
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
                _ => {}
            },
            _ = ticker.tick() => {
                (life.sink)(Event::Progress { downloaded: shared.downloaded.load(Ordering::Relaxed), total: Some(total) });
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
