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
- Quorix Bench, a tool that measures speed and integrity against a local server.

- Automatic mode for the number of connections. It starts with 4 and adds connections only while the measured speed increases.
- Block hashes. The progress file stores a BLAKE3 hash for each completed block. Before saved data is used again, each block is read from the disk and compared.
- Option `--no-recheck`, which uses saved data without the check.
- Hedged requests. A free connection requests a slow or stopped part again, if the server sends a strong `ETag`.
- Received data is written to the disk after a maximum of 0.5 seconds, also when the connection becomes silent.

### Changed

- The format of the progress file is version 2. A progress file from an earlier build is ignored, and the download starts from zero.
- Saved data is used again in full blocks only. Bytes of a block that was not complete are downloaded again.
- The option `-c` now sets a fixed number of connections. Without it, Quorix uses the automatic mode with a maximum of 16.

- The first request of a download asks for 256 KiB, not 1 MiB. On a slow server the large first part controlled the time of the full download.
- The minimum size of a part is 256 KiB, not 1 MiB. Small files can use all the connections that the user sets.

### Fixed

- The test server limited the speed only for range requests. A client without a range request got the file at full speed, which made comparisons incorrect.
- The test server built the full file for each range request.
