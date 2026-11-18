// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;

use quorix_http::HttpError;
use quorix_integrity::InvalidDigest;
use quorix_storage::StorageError;

/// Every way a download can fail. Each variant has a stable code
/// ([`Error::code`]) and a plain sentence (its `Display` text), so a user never
/// has to read a log to learn what happened (SOP 14.5).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The text is not a link.
    #[error("\"{0}\" is not a valid link")]
    InvalidUrl(String),
    /// Scheme other than http or https.
    #[error("{0} links are not supported, only http and https")]
    UnsupportedScheme(String),
    /// The destination path cannot be used.
    #[error("{0}")]
    InvalidPath(String),
    /// The checksum given by the user is malformed.
    #[error("{0}")]
    InvalidDigest(#[from] InvalidDigest),

    /// No connection could be made.
    #[error("could not connect to the server: {0}")]
    Connect(String),
    /// The TLS handshake or certificate check failed.
    #[error("the secure connection could not be verified: {0}")]
    Tls(String),
    /// The server went silent.
    #[error("the server did not answer in time")]
    Timeout,
    /// The connection broke mid-transfer.
    #[error("the connection closed before the download finished")]
    ConnectionClosed,
    /// Redirect limit reached.
    #[error("the server redirected too many times")]
    TooManyRedirects,
    /// https redirected to http.
    #[error("a secure link redirected to an insecure http address")]
    InsecureRedirect,
    /// A public address redirected to this computer or the local network.
    #[error("the server redirected to an address on this computer or the local network")]
    RedirectToLocal,

    /// The server answered with an error status.
    #[error("{}", http_sentence(*.status, *.retry_after))]
    Http {
        /// HTTP status code.
        status: u16,
        /// Seconds the server asked the client to wait, when it said so.
        retry_after: Option<u64>,
    },
    /// The server answered in a way that cannot be turned into a correct file.
    #[error("the server sent an unexpected answer: {0}")]
    Protocol(String),

    /// The destination exists and overwriting was not requested.
    #[error("{} already exists", .0.display())]
    DestinationExists(PathBuf),
    /// No space left.
    #[error("the disk is full")]
    DiskFull,
    /// Another file system failure.
    #[error("could not write the file: {0}")]
    Io(String),

    /// Fewer or more bytes arrived than the server announced.
    #[error("the server announced {expected} bytes but sent {received}")]
    LengthMismatch {
        /// Bytes announced in `Content-Length`.
        expected: u64,
        /// Bytes actually received.
        received: u64,
    },
    /// The finished file does not match the checksum the user supplied.
    #[error("the {algorithm} checksum does not match: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Algorithm name.
        algorithm: &'static str,
        /// Digest the user supplied.
        expected: String,
        /// Digest of the received data.
        actual: String,
    },

    /// The file on the server is no longer the version this download started with.
    #[error("the file changed on the server while it was being downloaded")]
    ResourceChanged,

    /// The user cancelled.
    #[error("the download was cancelled")]
    Cancelled,
    /// A defect in Quorix itself. Should never be seen; please report it.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Stable identifier for this failure, for example `QRX-E403`.
    ///
    /// Ranges: 1xx input, 2xx network, 3xx to 5xx the HTTP status itself,
    /// 6xx storage, 7xx integrity, 900 cancelled, 999 internal defect.
    pub fn code(&self) -> String {
        let n = match self {
            Error::InvalidUrl(_) => 101,
            Error::UnsupportedScheme(_) => 102,
            Error::InvalidPath(_) => 103,
            Error::InvalidDigest(_) => 104,
            Error::Connect(_) => 201,
            Error::Tls(_) => 202,
            Error::Timeout => 203,
            Error::ConnectionClosed => 204,
            Error::TooManyRedirects => 205,
            Error::InsecureRedirect => 206,
            Error::RedirectToLocal => 207,
            Error::Http { status, .. } => *status,
            Error::DestinationExists(_) => 601,
            Error::DiskFull => 602,
            Error::Io(_) => 603,
            Error::LengthMismatch { .. } => 701,
            Error::ChecksumMismatch { .. } => 702,
            Error::Protocol(_) => 703,
            Error::ResourceChanged => 704,
            Error::Cancelled => 900,
            Error::Internal(_) => 999,
        };
        format!("QRX-E{n}")
    }

    /// True when trying again later could succeed without the user changing anything.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Connect(_) | Error::Timeout | Error::ConnectionClosed => true,
            Error::Http { status, .. } => matches!(status, 408 | 429 | 500 | 502 | 503 | 504),
            _ => false,
        }
    }
}

fn http_sentence(status: u16, retry_after: Option<u64>) -> String {
    let text = match status {
        401 => "the server requires a sign-in for this file".to_string(),
        403 => "the server refused the request".to_string(),
        404 => "the file was not found on the server".to_string(),
        408 => "the server timed out waiting for the request".to_string(),
        410 => "the file is no longer available on the server".to_string(),
        416 => "the server could not provide the requested part of the file".to_string(),
        429 => match retry_after {
            Some(s) => format!("the server is limiting requests and asked to wait {s} seconds"),
            None => "the server is limiting requests".to_string(),
        },
        500..=599 => "the server had a problem of its own".to_string(),
        _ => "the server did not send the file".to_string(),
    };
    format!("{text} (HTTP {status})")
}

impl From<HttpError> for Error {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::InvalidUrl(s) => Error::InvalidUrl(s),
            HttpError::UnsupportedScheme(s) => Error::UnsupportedScheme(s),
            HttpError::TooManyRedirects => Error::TooManyRedirects,
            HttpError::InsecureRedirect => Error::InsecureRedirect,
            HttpError::RedirectToLocal => Error::RedirectToLocal,
            HttpError::Timeout => Error::Timeout,
            HttpError::Connect(s) => Error::Connect(s),
            HttpError::Tls(s) => Error::Tls(s),
            HttpError::Body(_) => Error::ConnectionClosed,
            HttpError::Other(s) => Error::Connect(s),
        }
    }
}

impl From<StorageError> for Error {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::DestinationExists(p) => Error::DestinationExists(p),
            StorageError::DiskFull => Error::DiskFull,
            StorageError::InvalidPath(s) => Error::InvalidPath(s),
            StorageError::Io(e) => Error::Io(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_errors_use_the_status_as_their_code() {
        let e = Error::Http { status: 403, retry_after: None };
        assert_eq!(e.code(), "QRX-E403");
        assert_eq!(e.to_string(), "the server refused the request (HTTP 403)");
        assert!(!e.is_retryable());
    }

    #[test]
    fn rate_limit_mentions_the_wait_and_is_retryable() {
        let e = Error::Http { status: 429, retry_after: Some(7) };
        assert_eq!(e.to_string(), "the server is limiting requests and asked to wait 7 seconds (HTTP 429)");
        assert!(e.is_retryable());
    }

    #[test]
    fn permanent_failures_are_not_retryable() {
        for e in [
            Error::Http { status: 404, retry_after: None },
            Error::DiskFull,
            Error::Cancelled,
            Error::Tls("bad certificate".into()),
            Error::LengthMismatch { expected: 2, received: 1 },
        ] {
            assert!(!e.is_retryable(), "{e}");
        }
    }
}
