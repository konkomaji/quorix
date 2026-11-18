// SPDX-License-Identifier: GPL-3.0-or-later

//! Quorix Core: the transfer engine shared by the desktop app, the CLI and the
//! browser integration.
//!
//! What it does today:
//! - **Segmented transfer.** When a server honours byte ranges, the file is
//!   fetched in pieces over several connections and written straight to its
//!   final offsets. Every `206` answer is checked against the range requested.
//! - **Sequential transfer.** Servers without range support get one plain request.
//! - **Resume.** Progress is checkpointed. An interrupted or paused download
//!   continues where it stopped, but only when the server proves, with a strong
//!   `ETag`, that the file is still the same version.
//! - **Retries.** Temporary failures are retried a bounded number of times with
//!   exponential backoff, honouring `Retry-After`.
//! - **Verified completion.** A file reaches its destination only after every
//!   byte is present and, when given, its checksum matches.
//!
//! ```no_run
//! # async fn demo() -> Result<(), quorix_core::Error> {
//! use quorix_core::{download, CancelToken, DownloadRequest, Outcome, Target};
//!
//! let request = DownloadRequest::new("https://example.com/file.zip", Target::Directory(".".into()));
//! match download(&request, &CancelToken::new(), &|_event| {}).await? {
//!     Outcome::Completed(done) => println!("saved {} bytes to {}", done.bytes, done.path.display()),
//!     Outcome::Paused { downloaded, total } => println!("paused at {downloaded} of {total}"),
//! }
//! # Ok(()) }
//! ```

mod cancel;
mod engine;
mod error;
mod segmented;
mod state;

pub use cancel::CancelToken;
pub use engine::{download, Completed, DownloadRequest, Event, Outcome, RetryPolicy, Target};
pub use error::Error;
pub use quorix_http::{redact_url, ClientConfig};
pub use quorix_integrity::{Algorithm, Expected, InvalidDigest};
pub use state::TaskState;
