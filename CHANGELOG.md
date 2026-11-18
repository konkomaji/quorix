# Changelog

This file records the important changes to Quorix.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Quorix uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Transfer engine `quorix-core` with a state machine of 12 states.
- Transfer with one connection for servers that do not support byte ranges.
- Transfer with 1 to 16 connections for servers that support byte ranges.
- Pause and resume. Resume needs a strong `ETag` from the server.
- Progress file that stays correct after a crash.
- Retries with a limit, an increasing wait time, and support for `Retry-After`.
- SHA-256 and BLAKE3 verification of the completed file.
- Error codes in the form `QRX-Exxx` with one sentence for each failure.
- Redirect rules: maximum of 10, no change from `https` to `http`, no change from a public address to a local address.
- Safe file names for names that a server sends.
- Sparse file allocation on Windows.
- Command-line tool `quorix`.
- Browser extension Quorix Connect for Chrome and Edge. It cannot connect at this time, because the desktop application does not exist.
- Shared visual theme with the Poppins font.
- Local test server that makes faults on purpose.
