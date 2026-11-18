// SPDX-License-Identifier: GPL-3.0-or-later

//! A small HTTP/1.1 server for tests. Each path produces one scripted
//! behaviour, including broken ones a real library would refuse to send.
//! It speaks raw TCP on purpose: fault injection needs to violate the protocol.
//!
//! Whole-file routes (they ignore `Range`):
//!
//! | Path | Behaviour |
//! |---|---|
//! | `/ok/<n>` | 200, `Content-Length: n`, `n` bytes of [`body`] |
//! | `/nolen/<n>` | 200, no length, body ends when the connection closes |
//! | `/redirect/<n>` | 302 to `/ok/<n>` |
//! | `/loop` | 302 to itself |
//! | `/status/<code>` | that status, empty body; 429 and 503 add `Retry-After: 7` |
//! | `/short/<n>` | announces `n` bytes, sends half, closes |
//! | `/long/<n>` | announces `n` bytes, sends `n + 16` |
//! | `/stall/<n>` | announces `n` bytes, sends half, then goes silent |
//! | `/partial/<n>` | 206 for bytes `0..n` of `2n`, whatever was asked |
//! | `/gzip` | 200 with `Content-Encoding: gzip` |
//! | `/named` | 200 with a `Content-Disposition` name that tries to escape: `../../evil.txt` |
//! | `/range-ignore/<n>` | claims `Accept-Ranges: bytes` but always sends the whole file |
//!
//! Range routes (they honour `Range` and `If-Range`, strong `ETag` `"v<version>"`):
//!
//! | Path | Behaviour |
//! |---|---|
//! | `/range/<n>` | correct |
//! | `/range-slow/<n>` | correct, throttled to a few hundred KiB per second per connection |
//! | `/range-half/<n>` | sends only the first half of each requested range, then closes |
//! | `/range-badcr/<n>` | `Content-Range` is shifted by one byte |
//! | `/range-one/<n>` | 403 for a ranged request while another one is being served |
//! | `/range-503/<n>` | the second and third requests get 503, the rest are correct |
//! | `/range-weak/<n>` | throttled like `range-slow`, but the `ETag` is weak |

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Deterministic test payload of `len` bytes. Not a repeating single byte, so
/// truncation, duplication and reordering all change the content.
pub fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(31).wrapping_add(i >> 8) % 251) as u8).collect()
}

/// Payload of resource version `version`. Version 1 is [`body`]; other versions differ in every byte.
pub fn body_of_version(len: usize, version: u32) -> Vec<u8> {
    let mask = if version == 1 { 0 } else { 0x5a };
    body(len).into_iter().map(|b| b ^ mask).collect()
}

#[derive(Default)]
struct State {
    unthrottled: AtomicU32,
    version: AtomicU32,
    hits: AtomicU32,
    serving: AtomicU32,
}

/// A running test server. It stops when dropped.
pub struct TestServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
    state: Arc<State>,
}

impl TestServer {
    /// Starts the server on a free local port.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(State { version: AtomicU32::new(1), ..State::default() });
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = Arc::clone(&shared);
                tokio::spawn(async move {
                    let _ = serve(stream, &state).await;
                });
            }
        });
        Self { addr, task, state }
    }

    /// Absolute URL for `path` on this server.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    /// Replaces the resource on the range routes: new content, new `ETag`.
    pub fn set_version(&self, version: u32) {
        self.state.version.store(version, Ordering::SeqCst);
    }

    /// Removes the speed limit from the throttled routes, so a test can pause
    /// on a slow server and finish quickly on the very same address.
    pub fn stop_throttling(&self) {
        self.state.unthrottled.store(1, Ordering::SeqCst);
    }

    /// Requests received so far.
    pub fn hits(&self) -> u32 {
        self.state.hits.load(Ordering::SeqCst)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type Headers<'a> = &'a [(&'a str, String)];

fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

/// Parses `bytes=a-b` into an inclusive range clamped to `len`.
fn requested_range(request: &str, len: usize) -> Option<(usize, usize)> {
    let (start, end) = header(request, "range")?.strip_prefix("bytes=")?.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let end: usize = end.parse::<usize>().ok()?.min(len.checked_sub(1)?);
    (start <= end).then_some((start, end))
}

async fn serve(mut stream: TcpStream, state: &State) -> std::io::Result<()> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).await?;
        if n == 0 || raw.len() > 16 * 1024 {
            return Ok(());
        }
        raw.extend_from_slice(&buf[..n]);
    }
    let request = String::from_utf8_lossy(&raw).into_owned();
    let hit = state.hits.fetch_add(1, Ordering::SeqCst) + 1;
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let (route, arg) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let n: usize = arg.parse().unwrap_or(0);
    let length = |len: usize| ("Content-Length", len.to_string());

    match route {
        "ok" => send(&mut stream, "200 OK", &[length(n), ("ETag", "\"v1\"".into())], &body(n)).await,
        "nolen" => send(&mut stream, "200 OK", &[], &body(n)).await,
        "redirect" => send(&mut stream, "302 Found", &[("Location", format!("/ok/{n}")), length(0)], b"").await,
        "loop" => send(&mut stream, "302 Found", &[("Location", "/loop".into()), length(0)], b"").await,
        "status" => {
            let mut headers = vec![length(0)];
            if n == 429 || n == 503 {
                headers.push(("Retry-After", "7".into()));
            }
            send(&mut stream, &format!("{n} Test"), &headers, b"").await
        }
        "short" => send(&mut stream, "200 OK", &[length(n)], &body(n)[..n / 2]).await,
        "long" => send(&mut stream, "200 OK", &[length(n)], &body(n + 16)).await,
        "stall" => {
            send(&mut stream, "200 OK", &[length(n)], &body(n)[..n / 2]).await?;
            tokio::time::sleep(Duration::from_secs(120)).await;
            Ok(())
        }
        "partial" => {
            let headers = [length(n), ("Content-Range", format!("bytes 0-{}/{}", n.saturating_sub(1), n * 2))];
            send(&mut stream, "206 Partial Content", &headers, &body(n)).await
        }
        "gzip" => send(&mut stream, "200 OK", &[length(20), ("Content-Encoding", "gzip".into())], &body(20)).await,
        "named" => {
            let headers = [length(16), ("Content-Disposition", "attachment; filename=\"../../evil.txt\"".into())];
            send(&mut stream, "200 OK", &headers, &body(16)).await
        }
        "range-ignore" => send(&mut stream, "200 OK", &[length(n), ("Accept-Ranges", "bytes".into())], &body(n)).await,
        "range" | "range-slow" | "range-half" | "range-badcr" | "range-one" | "range-503" | "range-weak" => {
            ranged(&mut stream, state, route, n, &request, hit).await
        }
        _ => send(&mut stream, "404 Not Found", &[length(0)], b"").await,
    }
}

async fn ranged(stream: &mut TcpStream, state: &State, route: &str, n: usize, request: &str, hit: u32) -> std::io::Result<()> {
    let version = state.version.load(Ordering::SeqCst);
    let data = body_of_version(n, version);
    let etag = if route == "range-weak" { format!("W/\"v{version}\"") } else { format!("\"v{version}\"") };
    let length = |len: usize| ("Content-Length", len.to_string());

    if route == "range-503" && (hit == 2 || hit == 3) {
        return send(stream, "503 Service Unavailable", &[length(0)], b"").await;
    }

    // If-Range names a version that is gone: answer with the whole current file.
    let version_matches = header(request, "if-range").is_none_or(|wanted| wanted == etag);
    let Some((start, end)) = requested_range(request, n).filter(|_| version_matches) else {
        return send(stream, "200 OK", &[length(n), ("ETag", etag)], &data).await;
    };

    if route == "range-one" && state.serving.fetch_add(1, Ordering::SeqCst) > 0 {
        state.serving.fetch_sub(1, Ordering::SeqCst);
        return send(stream, "403 Forbidden", &[length(0)], b"").await;
    }

    let shift = usize::from(route == "range-badcr");
    let headers = [
        length(end - start + 1),
        ("Content-Range", format!("bytes {}-{}/{n}", start + shift, end + shift)),
        ("ETag", etag),
        ("Accept-Ranges", "bytes".into()),
    ];
    let slice = &data[start..=end];
    let result = match route {
        "range-half" => send(stream, "206 Partial Content", &headers, &slice[..slice.len().div_ceil(2)]).await,
        "range-slow" | "range-one" | "range-weak" if state.unthrottled.load(Ordering::SeqCst) == 0 => {
            send(stream, "206 Partial Content", &headers, b"").await?;
            let mut result = Ok(());
            let last = slice.chunks(4096).count().saturating_sub(1);
            for (index, piece) in slice.chunks(4096).enumerate() {
                if route == "range-one" && index == last {
                    // Free the slot before the final bytes go out, so the client's next request cannot race it.
                    state.serving.fetch_sub(1, Ordering::SeqCst);
                }
                result = stream.write_all(piece).await;
                if result.is_err() {
                    break;
                }
                let _ = stream.flush().await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            result
        }
        _ => send(stream, "206 Partial Content", &headers, slice).await,
    };
    result
}

async fn send(stream: &mut TcpStream, status: &str, headers: Headers<'_>, payload: &[u8]) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}
