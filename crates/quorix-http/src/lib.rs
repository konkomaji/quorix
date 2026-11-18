// SPDX-License-Identifier: GPL-3.0-or-later

//! HTTP transport: client construction, a bounded redirect policy and helpers
//! for reading the response metadata the engine needs.
//!
//! This crate hides the HTTP library. Nothing outside it names a `reqwest` type.

use std::error::Error as StdError;
use std::fmt;
use std::time::Duration;

pub use bytes::Bytes;
pub use url::Url;

/// Transport settings.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Redirects followed before giving up.
    pub max_redirects: usize,
    /// Time allowed to establish a connection.
    pub connect_timeout: Duration,
    /// Longest silence tolerated while reading a response.
    pub read_timeout: Duration,
    /// Allow a redirect from `https` to `http`. Off by default.
    pub allow_insecure_redirect: bool,
    /// Use the proxy configured in the environment.
    pub use_system_proxy: bool,
    /// `User-Agent` header value.
    pub user_agent: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            max_redirects: 10,
            connect_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(60),
            allow_insecure_redirect: false,
            use_system_proxy: true,
            user_agent: concat!("Quorix/", env!("CARGO_PKG_VERSION")).to_string(),
        }
    }
}

/// Transport failures, before any HTTP status is interpreted.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HttpError {
    /// The text is not a URL.
    #[error("\"{0}\" is not a valid link")]
    InvalidUrl(String),
    /// The URL scheme is not `http` or `https`.
    #[error("{0} links are not supported, only http and https")]
    UnsupportedScheme(String),
    /// The redirect limit was reached.
    #[error("the server redirected too many times")]
    TooManyRedirects,
    /// A secure link redirected to an insecure one.
    #[error("a secure link redirected to an insecure http address")]
    InsecureRedirect,
    /// A public address redirected to this computer or the local network.
    #[error("the server redirected to an address on this computer or the local network")]
    RedirectToLocal,
    /// The server did not answer in time.
    #[error("the server did not answer in time")]
    Timeout,
    /// No connection could be made.
    #[error("could not connect to the server: {0}")]
    Connect(String),
    /// The TLS handshake or certificate check failed.
    #[error("the secure connection could not be verified: {0}")]
    Tls(String),
    /// The connection broke while the body was being read.
    #[error("the connection closed before the download finished: {0}")]
    Body(String),
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// Parses a link and checks that its scheme is allowed.
pub fn parse_url(raw: &str) -> Result<Url, HttpError> {
    // The text may hold an access token in its query string, so only a short, query-free form is ever shown.
    let shown = || raw.trim().split(['?', '#']).next().unwrap_or("").chars().take(80).collect::<String>();
    let url = Url::parse(raw.trim()).map_err(|_| HttpError::InvalidUrl(shown()))?;
    match url.scheme() {
        "http" | "https" if url.has_host() => Ok(url),
        "http" | "https" => Err(HttpError::InvalidUrl(shown())),
        other => Err(HttpError::UnsupportedScheme(other.to_string())),
    }
}

/// Why the redirect policy stopped a request. Carried inside the HTTP library's error.
#[derive(Debug)]
enum RedirectBlock {
    TooMany,
    Insecure,
    Local,
    Scheme(String),
}

impl fmt::Display for RedirectBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "redirect blocked: {self:?}")
    }
}

impl StdError for RedirectBlock {}

/// HTTP client. Cheap to clone; connections are pooled inside.
#[derive(Debug, Clone)]
pub struct Client {
    inner: reqwest::Client,
}

impl Client {
    /// Builds a client from `config`.
    pub fn new(config: &ClientConfig) -> Result<Self, HttpError> {
        let max = config.max_redirects;
        let allow_insecure = config.allow_insecure_redirect;
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > max {
                return attempt.error(RedirectBlock::TooMany);
            }
            let scheme = attempt.url().scheme();
            if scheme != "http" && scheme != "https" {
                let scheme = scheme.to_string();
                return attempt.error(RedirectBlock::Scheme(scheme));
            }
            let from_secure = attempt.previous().last().is_some_and(|u| u.scheme() == "https");
            if from_secure && scheme == "http" && !allow_insecure {
                return attempt.error(RedirectBlock::Insecure);
            }
            // A public site must not be able to point this client at services that
            // only this computer or its network can reach.
            let started_local = attempt.previous().first().is_some_and(is_local_address);
            if !started_local && is_local_address(attempt.url()) {
                return attempt.error(RedirectBlock::Local);
            }
            attempt.follow()
        });

        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(policy)
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .user_agent(config.user_agent.clone());
        if !config.use_system_proxy {
            builder = builder.no_proxy();
        }
        let inner = builder.build().map_err(|e| HttpError::Other(e.to_string()))?;
        Ok(Self { inner })
    }

    /// Sends a GET for the whole resource.
    ///
    /// `Accept-Encoding: identity` is requested so the bytes received are the
    /// bytes of the file, with no compression layer changing offsets or length.
    pub async fn get(&self, url: &Url) -> Result<Response, HttpError> {
        let inner = self.inner.get(url.clone()).header(reqwest::header::ACCEPT_ENCODING, "identity").send().await.map_err(classify)?;
        Ok(Response { inner })
    }

    /// Sends a GET for bytes `start..=end_inclusive`.
    ///
    /// With `if_range` (a strong `ETag`) the server sends the range only when
    /// the resource still has that version. If it changed, the server answers
    /// `200` with the whole new file, and the caller must not treat that as the range.
    pub async fn get_range(&self, url: &Url, start: u64, end_inclusive: u64, if_range: Option<&str>) -> Result<Response, HttpError> {
        let mut request = self
            .inner
            .get(url.clone())
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header(reqwest::header::RANGE, format!("bytes={start}-{end_inclusive}"));
        if let Some(etag) = if_range {
            request = request.header(reqwest::header::IF_RANGE, etag);
        }
        Ok(Response { inner: request.send().await.map_err(classify)? })
    }
}

/// True when the URL points at this computer or a private network: `localhost`,
/// loopback, private, link-local, unique-local or unspecified addresses.
/// Only literal addresses and the `localhost` names are recognised; a public
/// name that resolves to a private address is not detected here.
pub fn is_local_address(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast(),
        Some(url::Host::Ipv6(ip)) => {
            let first = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link local fe80::/10
                || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified())
        }
        None => false,
    }
}

/// A URL that is safe to show or log: no user name or password, and every
/// query value replaced, because query strings often carry access tokens.
pub fn redact_url(url: &Url) -> String {
    let mut shown = url.clone();
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    shown.set_fragment(None);
    if url.query().is_some() {
        let keys: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
        shown.set_query(None);
        let mut text = shown.to_string();
        text.push('?');
        text.push_str(&keys.iter().map(|k| format!("{k}=***")).collect::<Vec<_>>().join("&"));
        return text;
    }
    shown.to_string()
}

/// A parsed `Content-Range: bytes start-end/total` header. `end` is inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    /// First byte of the range.
    pub start: u64,
    /// Last byte of the range, inclusive.
    pub end: u64,
    /// Size of the whole resource, when the server states it.
    pub total: Option<u64>,
}

/// Parses a `Content-Range` value. Returns `None` for anything that is not a
/// well-formed, satisfiable byte range: wrong unit, `start > end`, `end >= total`.
pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.trim().split_once('-')?;
    let (start, end): (u64, u64) = (start.trim().parse().ok()?, end.trim().parse().ok()?);
    let total = match total.trim() {
        "*" => None,
        n => Some(n.parse::<u64>().ok()?),
    };
    if start > end || total.is_some_and(|t| end >= t) {
        return None;
    }
    Some(ContentRange { start, end, total })
}

/// A response whose headers have arrived. The body is read with [`Response::chunk`].
#[derive(Debug)]
pub struct Response {
    inner: reqwest::Response,
}

impl Response {
    /// HTTP status code.
    pub fn status(&self) -> u16 {
        self.inner.status().as_u16()
    }

    /// URL that produced this response, after redirects.
    pub fn final_url(&self) -> &Url {
        self.inner.url()
    }

    /// A header as text, when present and readable.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.inner.headers().get(name)?.to_str().ok()
    }

    /// `Content-Length`, when the server declared one.
    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length")?.trim().parse().ok()
    }

    /// `Content-Encoding` other than `identity`, when the server applied one.
    pub fn content_encoding(&self) -> Option<&str> {
        self.header("content-encoding").map(str::trim).filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("identity"))
    }

    /// `ETag` validator.
    pub fn etag(&self) -> Option<&str> {
        self.header("etag")
    }

    /// The `ETag` when it is a strong validator: quoted and not prefixed with `W/`.
    /// Only a strong validator proves two responses hold byte-identical data.
    pub fn strong_etag(&self) -> Option<&str> {
        self.etag().map(str::trim).filter(|e| e.len() >= 2 && e.starts_with('"') && e.ends_with('"'))
    }

    /// The parsed `Content-Range` header.
    pub fn content_range(&self) -> Option<ContentRange> {
        parse_content_range(self.header("content-range")?)
    }

    /// `Last-Modified` validator.
    pub fn last_modified(&self) -> Option<&str> {
        self.header("last-modified")
    }

    /// `Retry-After` in whole seconds. The HTTP-date form is not handled yet.
    pub fn retry_after_secs(&self) -> Option<u64> {
        self.header("retry-after")?.trim().parse().ok()
    }

    /// File name suggested by the server or, failing that, by the URL.
    /// The value is untrusted: sanitize it before using it as a path.
    pub fn suggested_file_name(&self) -> Option<String> {
        self.header("content-disposition").and_then(file_name_from_content_disposition).or_else(|| {
            let last = self.final_url().path_segments()?.rfind(|s| !s.is_empty())?;
            Some(percent_decode(last)).filter(|s| !s.is_empty())
        })
    }

    /// Next piece of the body, or `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, HttpError> {
        self.inner.chunk().await.map_err(classify)
    }
}

fn classify(e: reqwest::Error) -> HttpError {
    let mut chain = String::new();
    let mut root_cause = String::new(); // the deepest error says what actually went wrong
    let mut source: Option<&(dyn StdError + 'static)> = Some(&e);
    while let Some(err) = source {
        match err.downcast_ref::<RedirectBlock>() {
            Some(RedirectBlock::TooMany) => return HttpError::TooManyRedirects,
            Some(RedirectBlock::Insecure) => return HttpError::InsecureRedirect,
            Some(RedirectBlock::Local) => return HttpError::RedirectToLocal,
            Some(RedirectBlock::Scheme(s)) => return HttpError::UnsupportedScheme(s.clone()),
            None => {}
        }
        if !chain.is_empty() {
            chain.push_str(": ");
        }
        chain.push_str(&err.to_string());
        root_cause = err.to_string();
        source = err.source();
    }

    // The library has no "TLS failed" flag, so the cause chain is searched for it.
    let lower = chain.to_ascii_lowercase();
    let detail = root_cause;
    if e.is_timeout() {
        HttpError::Timeout
    } else if lower.contains("certificate") || lower.contains("tls") || lower.contains("handshake") {
        HttpError::Tls(detail)
    } else if e.is_connect() {
        HttpError::Connect(detail)
    } else if e.is_body() || e.is_decode() {
        HttpError::Body(detail)
    } else {
        HttpError::Other(chain)
    }
}

/// Decodes `%XX` escapes. Invalid escapes are kept as written.
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads the file name from a `Content-Disposition` header.
///
/// `filename*=UTF-8''…` wins over `filename="…"`. Parameters are split on `;`,
/// so a quoted name that itself contains `;` is cut short. That is rare and the
/// result is still a safe name after sanitizing.
pub fn file_name_from_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').skip(1) {
        let Some((key, val)) = part.split_once('=') else { continue };
        let (key, val) = (key.trim().to_ascii_lowercase(), val.trim());
        if key == "filename*" {
            let mut pieces = val.splitn(3, '\'');
            if let (Some(charset), Some(_lang), Some(encoded)) = (pieces.next(), pieces.next(), pieces.next()) {
                if charset.eq_ignore_ascii_case("utf-8") {
                    let decoded = percent_decode(encoded);
                    if !decoded.is_empty() {
                        return Some(decoded);
                    }
                }
            }
        } else if key == "filename" {
            plain = Some(val.trim_matches('"').to_string());
        }
    }
    plain.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_and_https_only() {
        assert!(parse_url("https://example.com/file.zip").is_ok());
        assert!(parse_url("  http://example.com/a  ").is_ok());
        assert_eq!(parse_url("ftp://example.com/a"), Err(HttpError::UnsupportedScheme("ftp".into())));
        assert_eq!(parse_url("file:///etc/passwd"), Err(HttpError::UnsupportedScheme("file".into())));
        assert!(matches!(parse_url("not a link"), Err(HttpError::InvalidUrl(_))));
        assert!(matches!(parse_url(""), Err(HttpError::InvalidUrl(_))));
    }

    #[test]
    fn invalid_link_text_never_shows_a_query_string() {
        let Err(HttpError::InvalidUrl(shown)) = parse_url("not a link?token=SECRET") else { panic!() };
        assert_eq!(shown, "not a link");
    }

    #[test]
    fn parses_content_range_strictly() {
        let p = parse_content_range;
        assert_eq!(p("bytes 0-99/200"), Some(ContentRange { start: 0, end: 99, total: Some(200) }));
        assert_eq!(p("bytes 100-199/*"), Some(ContentRange { start: 100, end: 199, total: None }));
        assert_eq!(p("  bytes  5-5/6 "), Some(ContentRange { start: 5, end: 5, total: Some(6) }));
        let bad = [
            "",
            "0-99/200",
            "items 0-99/200",
            "bytes 99-0/200",
            "bytes 0-200/200",
            "bytes */200",
            "bytes 0-99",
            "bytes a-b/c",
            "bytes -5/10",
            "bytes 0-99/-1",
            "bytes 0-18446744073709551616/9",
        ];
        for value in bad {
            assert_eq!(p(value), None, "input {value:?}");
        }
    }

    #[test]
    fn recognises_local_addresses() {
        let local = |s: &str| is_local_address(&Url::parse(s).unwrap());
        let locals = [
            "http://localhost/",
            "http://LOCALHOST.:8080/",
            "http://app.localhost/",
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
        ];
        for url in locals {
            assert!(local(url), "{url} should be local");
        }
        for url in ["https://example.com/", "http://8.8.8.8/", "http://172.32.0.1/", "http://[2001:db8::1]/", "http://notlocalhost/"] {
            assert!(!local(url), "{url} should be public");
        }
    }

    #[test]
    fn redacts_credentials_and_query_values() {
        let r = |s: &str| redact_url(&Url::parse(s).unwrap());
        assert_eq!(r("https://example.com/a/file.zip"), "https://example.com/a/file.zip");
        assert_eq!(r("https://user:pass@example.com/f?token=SECRET&x=1#frag"), "https://example.com/f?token=***&x=***");
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("my%20file%281%29.zip"), "my file(1).zip");
        assert_eq!(percent_decode("r%C3%A9sum%C3%A9.pdf"), "résumé.pdf");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("bad%zzescape%2"), "bad%zzescape%2");
    }

    #[test]
    fn reads_content_disposition() {
        let f = file_name_from_content_disposition;
        assert_eq!(f("attachment; filename=\"report.pdf\""), Some("report.pdf".into()));
        assert_eq!(f("attachment; filename=plain.bin"), Some("plain.bin".into()));
        assert_eq!(f("attachment; filename=\"old.txt\"; filename*=UTF-8''na%C3%AFve.txt"), Some("naïve.txt".into()));
        assert_eq!(f("attachment; FILENAME=\"Upper.txt\""), Some("Upper.txt".into()));
        assert_eq!(f("inline"), None);
        assert_eq!(f("attachment; filename=\"\""), None);
    }
}
