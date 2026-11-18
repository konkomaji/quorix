// SPDX-License-Identifier: GPL-3.0-or-later

//! One download from start to finish: inspect, choose a transfer mode, verify, finalize.

use std::hash::{BuildHasher, Hasher as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use quorix_http::{parse_url, Client, ClientConfig, Response, Url};
use quorix_integrity::{Algorithm, Expected, Hasher};
use quorix_state::{Checkpoint, Ranges};
use quorix_storage::{sanitize_file_name, state_path_for, PartFile};

use crate::segmented::{self, Ended, Plan};
use crate::{CancelToken, Error, TaskState};

pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Bytes collected in memory before one positional write. Bounds memory per connection.
pub(crate) const WRITE_BUFFER: usize = 512 * 1024;
const MIB: u64 = 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;

/// Where the file goes.
#[derive(Debug, Clone)]
pub enum Target {
    /// Exactly this path.
    File(PathBuf),
    /// This directory, with a name taken from the server or the link.
    Directory(PathBuf),
}

/// How temporary failures are retried.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Failures in a row, with no bytes received in between, before giving up. Zero disables retries.
    pub max_retries: u32,
    /// Wait before the first retry. Doubles with each further failure.
    pub base_delay: Duration,
    /// Longest wait the backoff may reach.
    pub max_delay: Duration,
    /// Longest `Retry-After` the engine will honour. A longer request ends the download with the server's error.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(120),
        }
    }
}

impl RetryPolicy {
    /// Wait before retry number `attempt` (1 for the first). Uses the server's
    /// `Retry-After` when present, otherwise exponential backoff with jitter so
    /// that many clients do not retry in step. `None` means: do not retry.
    pub(crate) fn delay(&self, attempt: u32, error: &Error) -> Option<Duration> {
        if !error.is_retryable() || attempt > self.max_retries {
            return None;
        }
        if let Error::Http { retry_after: Some(seconds), .. } = error {
            let asked = Duration::from_secs(*seconds);
            return (asked <= self.max_retry_after).then_some(asked);
        }
        let doubled = self.base_delay.saturating_mul(1u32 << (attempt - 1).min(16));
        let capped = doubled.min(self.max_delay);
        Some(capped.mul_f64(0.5 + 0.5 * random_unit()))
    }
}

/// A number in `[0, 1)`. Not for security: it only spreads retry times.
fn random_unit() -> f64 {
    let bits = std::collections::hash_map::RandomState::new().build_hasher().finish();
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// One download to perform.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// Link to fetch. Only http and https are accepted.
    pub url: String,
    /// Destination.
    pub target: Target,
    /// Replace an existing file at the destination.
    pub overwrite: bool,
    /// Checksum the finished file must match.
    pub expected: Option<Expected>,
    /// Transport settings.
    pub http: ClientConfig,
    /// Most connections to use at once, from 1 to 16.
    pub connections: usize,
    /// Continue from a partial file left by an earlier run when that is provably safe.
    pub resume: bool,
    /// How temporary failures are retried.
    pub retry: RetryPolicy,
    /// How often confirmed progress is saved to disk.
    pub checkpoint_interval: Duration,
    /// Size of one range request. `None` picks a size from the file size and the connection count.
    pub piece_size: Option<u64>,
}

impl DownloadRequest {
    /// A request with default settings: 4 connections, resume on, no overwrite, no checksum.
    pub fn new(url: impl Into<String>, target: Target) -> Self {
        Self {
            url: url.into(),
            target,
            overwrite: false,
            expected: None,
            http: ClientConfig::default(),
            connections: 4,
            resume: true,
            retry: RetryPolicy::default(),
            checkpoint_interval: Duration::from_secs(2),
            piece_size: None,
        }
    }

    fn connection_limit(&self) -> usize {
        self.connections.clamp(1, MAX_CONNECTIONS)
    }

    /// Piece size for a file of `total` bytes: small enough that a slow
    /// connection holds little work, large enough that request overhead stays negligible.
    fn piece_size_for(&self, total: u64) -> u64 {
        self.piece_size.unwrap_or_else(|| (total / (self.connection_limit() as u64 * 4)).clamp(MIB / 4, 16 * MIB)).max(1)
    }
}

/// Result of a successful download.
#[derive(Debug, Clone)]
pub struct Completed {
    /// Final location of the file.
    pub path: PathBuf,
    /// Size of the file in bytes.
    pub bytes: u64,
    /// Bytes that were already on disk from an earlier run.
    pub resumed: u64,
    /// Checksum that was verified, when one was requested.
    pub digest: Option<(Algorithm, String)>,
    /// Address the data came from, after redirects. May contain secrets; use [`crate::redact_url`] before showing it.
    pub final_url: String,
    /// `ETag` the server sent, for later comparison.
    pub etag: Option<String>,
}

/// How a download ended without an error.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The file is complete, verified and at its destination.
    Completed(Completed),
    /// The download was paused. The partial file and its checkpoint are kept;
    /// running the same request again continues from here.
    Paused {
        /// Bytes safely on disk.
        downloaded: u64,
        /// Size of the complete file.
        total: u64,
    },
}

impl Outcome {
    /// The completed download, or `None` when it was paused.
    pub fn completed(self) -> Option<Completed> {
        match self {
            Outcome::Completed(done) => Some(done),
            Outcome::Paused { .. } => None,
        }
    }
}

/// Something that happened during a download.
#[derive(Debug, Clone)]
pub enum Event {
    /// The task entered a new state.
    State(TaskState),
    /// The server answered and the destination is known.
    Started {
        /// Final location of the file.
        path: PathBuf,
        /// Size announced by the server, when it announced one.
        total: Option<u64>,
        /// Bytes already on disk from an earlier run.
        resumed: u64,
        /// Connections that will be used.
        connections: usize,
    },
    /// Bytes written so far.
    Progress {
        /// Bytes written to the part file.
        downloaded: u64,
        /// Size announced by the server, when it announced one.
        total: Option<u64>,
    },
}

pub(crate) type Sink<'a> = &'a (dyn Fn(Event) + Send + Sync);

/// Tracks the task state and refuses transitions the state machine forbids.
pub(crate) struct Lifecycle<'a> {
    state: TaskState,
    pub(crate) sink: Sink<'a>,
}

impl<'a> Lifecycle<'a> {
    fn new(sink: Sink<'a>) -> Self {
        sink(Event::State(TaskState::Created));
        Self { state: TaskState::Created, sink }
    }

    pub(crate) fn state(&self) -> TaskState {
        self.state
    }

    pub(crate) fn enter(&mut self, next: TaskState) {
        assert!(self.state.can_become(next), "engine bug: {} cannot become {next}", self.state);
        self.state = next;
        (self.sink)(Event::State(next));
    }
}

/// Downloads one file.
///
/// On success the file is at its destination, has the announced length and
/// matches the expected checksum when one was given. After a failure nothing is
/// at the destination. Partial data is kept only when a later run can safely
/// continue from it; otherwise it is deleted.
pub async fn download(request: &DownloadRequest, cancel: &CancelToken, on_event: Sink<'_>) -> Result<Outcome, Error> {
    let mut life = Lifecycle::new(on_event);
    let result = run(request, cancel, &mut life).await;
    if let Err(e) = &result {
        life.enter(if matches!(e, Error::Cancelled) { TaskState::Cancelled } else { TaskState::Failed });
    }
    result
}

/// What the first answer from the server tells us.
enum Inspected {
    /// The server honours ranges. `first` already carries bytes `0..first_end`.
    Ranged { first: Response, first_end: u64, total: u64 },
    /// The server sent the whole file in one answer.
    Whole(Response),
}

async fn run(request: &DownloadRequest, cancel: &CancelToken, life: &mut Lifecycle<'_>) -> Result<Outcome, Error> {
    life.enter(TaskState::Inspecting);
    let url = parse_url(&request.url)?;
    let client = Client::new(&request.http)?;
    let inspected = inspect(&client, &url, request, cancel).await?;

    let response = match &inspected {
        Inspected::Ranged { first, .. } => first,
        Inspected::Whole(response) => response,
    };
    let final_url = response.final_url().to_string();
    let etag = response.etag().map(str::to_string);
    let dest = match &request.target {
        Target::File(path) => path.clone(),
        Target::Directory(dir) => dir.join(sanitize_file_name(&response.suggested_file_name().unwrap_or_default())),
    };

    life.enter(TaskState::Queued);
    life.enter(TaskState::Downloading);

    let (part, bytes, resumed) = match inspected {
        Inspected::Whole(response) => {
            let part = stream(response, &dest, request, cancel, life).await?;
            let bytes = part.size().await?;
            (part, bytes, 0)
        }
        Inspected::Ranged { first, first_end, total } => {
            let strong = first.strong_etag().map(str::to_string);
            let identity = identity_of(&url);
            let (part, done) = open_part(&dest, request, total, strong.as_deref(), &identity).await?;
            let resumed = done.covered();
            let pieces = request.piece_size_for(total);
            // The first answer already holds the start of the file. Use it unless those bytes are on disk.
            let first = (resumed == 0).then_some((first, 0, first_end));
            let plan = Plan {
                client,
                url,
                part: part.clone(),
                total,
                strong_etag: strong,
                validator: etag.clone(),
                identity,
                done,
                first,
                piece_size: pieces,
                connections: request.connection_limit(),
                retry: request.retry.clone(),
                checkpoint_interval: request.checkpoint_interval,
            };
            (life.sink)(Event::Started { path: dest.clone(), total: Some(total), resumed, connections: plan.worker_count() });
            match segmented::run(plan, cancel, life).await {
                Ended::Complete => (part, total, resumed),
                Ended::Paused { downloaded } => {
                    drop(part); // keep the part file and its checkpoint on disk
                    return Ok(Outcome::Paused { downloaded, total });
                }
                Ended::Failed { error, keep } => {
                    if keep {
                        drop(part);
                    } else {
                        let _ = part.discard().await;
                    }
                    return Err(error);
                }
            }
        }
    };

    life.enter(TaskState::Verifying);
    let digest = match verify(&part, request.expected.as_ref(), bytes).await {
        Ok(digest) => digest,
        Err(e) => {
            let _ = part.discard().await;
            return Err(e);
        }
    };

    life.enter(TaskState::Finalizing);
    let path = part.finalize(request.overwrite).await?;
    life.enter(TaskState::Completed);
    Ok(Outcome::Completed(Completed { path, bytes, resumed, digest, final_url, etag }))
}

/// Asks for the first piece of the file with a range request and reads the
/// server's capabilities from how it answers. A header such as `Accept-Ranges`
/// is a claim; a correct `206` is proof. The answer is not wasted: its body is
/// the first piece of the download.
async fn inspect(client: &Client, url: &Url, request: &DownloadRequest, cancel: &CancelToken) -> Result<Inspected, Error> {
    // Small on purpose: on a slow server one large first piece would set the time of the whole download.
    let probe_end = request.piece_size.unwrap_or(MIB / 4).clamp(1, MIB) - 1;
    let mut failures = 0;
    loop {
        let attempt = async {
            let response = client.get_range(url, 0, probe_end, None).await?;
            let response = match response.status() {
                // An empty file cannot satisfy any range. Ask for it plainly.
                416 => client.get(url).await?,
                _ => response,
            };
            classify_first_answer(response, probe_end)
        };
        let result = tokio::select! {
            biased;
            _ = cancel.stopped() => return Err(Error::Cancelled),
            r = attempt => r,
        };
        match result {
            Ok(inspected) => return Ok(inspected),
            Err(error) => {
                failures += 1;
                let Some(delay) = request.retry.delay(failures, &error) else { return Err(error) };
                tokio::select! {
                    biased;
                    _ = cancel.stopped() => return Err(Error::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

fn classify_first_answer(response: Response, probe_end: u64) -> Result<Inspected, Error> {
    if let Some(encoding) = response.content_encoding() {
        // Saving these bytes would store a compressed stream under the file's name.
        return Err(Error::Protocol(format!("the file was sent with {encoding} encoding although none was requested")));
    }
    match response.status() {
        200 => Ok(Inspected::Whole(response)),
        206 => {
            let range =
                response.content_range().ok_or_else(|| Error::Protocol("a partial answer came without a valid Content-Range".into()))?;
            match range.total {
                Some(total) if range.start == 0 && range.end <= probe_end => {
                    Ok(Inspected::Ranged { first: response, first_end: range.end + 1, total })
                }
                Some(_) => {
                    Err(Error::Protocol(format!("bytes 0-{probe_end} were requested but bytes {}-{} were sent", range.start, range.end)))
                }
                None => Err(Error::Protocol("a partial answer did not state the size of the file".into())),
            }
        }
        status => Err(Error::Http { status, retry_after: response.retry_after_secs() }),
    }
}

/// Identifies the resource by scheme, host, port and path. The query string is
/// left out on purpose: signed links change their query on every visit while
/// pointing at the same file, and the server's `ETag` is what proves the version.
fn identity_of(url: &Url) -> String {
    let mut hasher = Hasher::new(Algorithm::Sha256);
    hasher.update(
        format!("{}://{}:{}{}", url.scheme(), url.host_str().unwrap_or(""), url.port_or_known_default().unwrap_or(0), url.path())
            .as_bytes(),
    );
    hasher.finalize_hex()
}

/// Opens the part file: the one left by an earlier run when it can be trusted, a fresh one otherwise.
///
/// Resume policy (SOP 6.2). Old data is reused only when all of these hold:
/// the caller allows resume; the server sent a strong `ETag`; a checkpoint
/// exists, parses and is internally consistent; it names the same resource,
/// size and `ETag`; and the part file is a regular file of exactly that size.
/// Anything less and the download starts again from zero. A weak `ETag` or no
/// `ETag` is never accepted as proof.
async fn open_part(
    dest: &Path,
    request: &DownloadRequest,
    total: u64,
    strong_etag: Option<&str>,
    identity: &str,
) -> Result<(PartFile, Ranges), Error> {
    if let (true, Some(etag)) = (request.resume, strong_etag) {
        let state_path = state_path_for(dest)?;
        let saved = tokio::task::spawn_blocking(move || Checkpoint::load(&state_path)).await.ok().flatten();
        if let Some(saved) = saved.filter(|c| c.identity == identity && c.total == total && c.etag == etag) {
            let done = saved.ranges();
            if done.covered() > 0 {
                if let Some(part) = PartFile::reopen(dest, request.overwrite, total).await? {
                    return Ok((part, done));
                }
            }
        }
    }
    Ok((PartFile::create(dest, request.overwrite, Some(total)).await?, Ranges::new()))
}

/// Sequential transfer: one answer, written from the start. Used when the
/// server does not honour ranges. It cannot be resumed, so any stop discards it.
async fn stream(
    mut response: Response,
    dest: &Path,
    request: &DownloadRequest,
    cancel: &CancelToken,
    life: &mut Lifecycle<'_>,
) -> Result<PartFile, Error> {
    let total = response.content_length();
    let part = PartFile::create(dest, request.overwrite, None).await?;
    (life.sink)(Event::Started { path: dest.to_path_buf(), total, resumed: 0, connections: 1 });

    let result = async {
        let mut buffer = Vec::with_capacity(WRITE_BUFFER);
        let mut written = 0u64;
        let mut last_report = Instant::now();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancel.stopped() => return Err(Error::Cancelled),
                c = response.chunk() => c?,
            };
            let Some(chunk) = chunk else { break };
            let received = written + buffer.len() as u64 + chunk.len() as u64;
            if let Some(expected) = total.filter(|t| received > *t) {
                return Err(Error::LengthMismatch { expected, received });
            }
            buffer.extend_from_slice(&chunk);
            if buffer.len() >= WRITE_BUFFER {
                let n = buffer.len() as u64;
                buffer = part.write_at(written, buffer).await?;
                written += n;
            }
            if last_report.elapsed() >= PROGRESS_INTERVAL {
                last_report = Instant::now();
                (life.sink)(Event::Progress { downloaded: written + buffer.len() as u64, total });
            }
        }
        let n = buffer.len() as u64;
        part.write_at(written, buffer).await?;
        written += n;
        (life.sink)(Event::Progress { downloaded: written, total });
        match total {
            Some(expected) if expected != written => Err(Error::LengthMismatch { expected, received: written }),
            _ => Ok(()),
        }
    }
    .await;

    match result {
        Ok(()) => Ok(part),
        Err(e) => {
            let _ = part.discard().await;
            Err(e)
        }
    }
}

/// Final checks before the file may be called complete: its length on disk,
/// and its checksum when the caller supplied one. The checksum is computed by
/// reading the file back, so it covers what is really on disk, not what was
/// believed to be written.
async fn verify(part: &PartFile, expected: Option<&Expected>, bytes: u64) -> Result<Option<(Algorithm, String)>, Error> {
    let on_disk = part.size().await?;
    if on_disk != bytes {
        return Err(Error::LengthMismatch { expected: bytes, received: on_disk });
    }
    let Some(expected) = expected else { return Ok(None) };

    let (path, algorithm) = (part.part_path().to_path_buf(), expected.algorithm());
    let actual = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Hasher::new(algorithm);
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            match file.read(&mut buffer)? {
                0 => return Ok(hasher.finalize_hex()),
                n => hasher.update(&buffer[..n]),
            }
        }
    })
    .await
    .map_err(|e| Error::Internal(e.to_string()))?
    .map_err(|e| Error::Io(e.to_string()))?;

    if expected.matches(&actual) {
        Ok(Some((algorithm, actual)))
    } else {
        Err(Error::ChecksumMismatch { algorithm: algorithm.name(), expected: expected.hex().to_string(), actual })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_stays_within_bounds_and_stops_at_the_limit() {
        let policy = RetryPolicy {
            max_retries: 4,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(350),
            ..Default::default()
        };
        let within = |attempt: u32, low: u64, high: u64| {
            for _ in 0..50 {
                let d = policy.delay(attempt, &Error::Timeout).unwrap().as_millis() as u64;
                assert!((low..=high).contains(&d), "attempt {attempt}: {d} ms not in {low}..={high}");
            }
        };
        within(1, 50, 100);
        within(2, 100, 200);
        within(3, 175, 350); // 400 capped to 350
        within(4, 175, 350);
        assert_eq!(policy.delay(5, &Error::Timeout), None, "retry budget spent");
    }

    #[test]
    fn permanent_errors_are_never_retried() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay(1, &Error::Http { status: 404, retry_after: None }), None);
        assert_eq!(policy.delay(1, &Error::DiskFull), None);
        assert_eq!(policy.delay(1, &Error::Tls("bad certificate".into())), None);
    }

    #[test]
    fn retry_after_is_honoured_exactly_or_refused_when_too_long() {
        let policy = RetryPolicy::default();
        let limited = |seconds| Error::Http { status: 429, retry_after: Some(seconds) };
        assert_eq!(policy.delay(1, &limited(7)), Some(Duration::from_secs(7)));
        assert_eq!(policy.delay(1, &limited(120)), Some(Duration::from_secs(120)));
        assert_eq!(policy.delay(1, &limited(121)), None, "longer than the engine is willing to wait");
    }

    #[test]
    fn identity_ignores_the_query_but_not_the_path_or_host() {
        let id = |s: &str| identity_of(&Url::parse(s).unwrap());
        assert_eq!(id("https://example.com/f.zip?sig=aaa"), id("https://example.com/f.zip?sig=bbb"));
        assert_eq!(id("https://example.com/f.zip"), id("https://example.com:443/f.zip"));
        assert_ne!(id("https://example.com/f.zip"), id("https://example.com/g.zip"));
        assert_ne!(id("https://example.com/f.zip"), id("https://example.org/f.zip"));
        assert_ne!(id("https://example.com/f.zip"), id("http://example.com/f.zip"));
    }

    #[test]
    fn piece_size_scales_with_the_file_and_stays_bounded() {
        let request = DownloadRequest::new("https://example.com/f", Target::Directory(".".into()));
        assert_eq!(request.piece_size_for(MIB), MIB / 4);
        assert_eq!(request.piece_size_for(16 * MIB), MIB);
        assert_eq!(request.piece_size_for(160 * MIB), 10 * MIB);
        assert_eq!(request.piece_size_for(100_000 * MIB), 16 * MIB);
    }
}
