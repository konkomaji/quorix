# QUORIX
## Software Development Standard Operating Procedure (SOP)
### Product Requirements, System Architecture, Engineering Standards and Release Plan

**Document ID:** QRX-ENG-SOP-001  
**Version:** 1.0  
**Date:** 18 November 2026  
**Project:** Quorix Download Manager  
**Status:** Proposed engineering specification  
**Product type:** Open-source desktop download manager and reusable download engine

---

# 1. Product Vision

## 1.1 Mission

Build Quorix as a reliable, high-performance and easy-to-use download manager.

Quorix must make file downloading simple for ordinary users while providing advanced control for technical users.

It must combine the practical strengths of Internet Download Manager (IDM), aria2, Gopeed, AB Download Manager, Motrix and specialized media downloaders without inheriting unnecessary interface complexity.

The product must prioritize:

1. File integrity.
2. Reliable download recovery.
3. Effective use of available network bandwidth.
4. Low resource consumption.
5. Simple and predictable operation.
6. Browser integration.
7. Security and privacy.
8. Extensibility and maintainability.

Speed is important, but correctness takes priority.

Quorix must never report a download as complete when required data is missing, inconsistent or corrupted.

## 1.2 Product statement

**Quorix is an open-source download manager designed to make fast, reliable and controlled downloading accessible through a minimal interface.**

Its performance advantage must come from better transfer scheduling, reduced overhead, efficient disk operations and recovery engineering.

It must not depend on misleading acceleration claims.

## 1.3 Long-term goal

Develop Quorix into three interoperable products:

- **Quorix Desktop:** The graphical download manager.
- **Quorix Core:** A reusable network transfer engine.
- **Quorix CLI:** A command-line application built on Quorix Core.

Supporting products can include Quorix Connect for browser integration and Quorix Bench for performance testing.

All interfaces must share the same transfer logic. Do not develop separate download engines for the desktop, CLI and browser components.

---

# 2. Product Principles

## 2.1 Reliability before speed

The primary requirement is that a downloaded file must match the correct remote resource.

When the original source provides a trustworthy checksum, Quorix must support verification against that checksum.

When no trusted checksum exists, Quorix must verify transfer completeness and HTTP response consistency. It must not claim to have proved that the source content itself is authentic.

## 2.2 Minimal by default

An ordinary user must be able to start a download by providing a URL and selecting a destination.

Advanced controls must remain available without overwhelming the main interface.

The default interface must show:

- Filename.
- File size.
- Downloaded bytes.
- Progress.
- Transfer speed.
- Estimated remaining time.
- State.
- Pause, resume and cancel actions.

Technical information must be accessible through a details panel.

## 2.3 Automatic optimization

The engine must make reasonable connection and scheduling decisions automatically.

Manual connection limits must remain available.

The automatic mode must be the recommended default.

## 2.4 Offline-first application design

Quorix must not require an account, cloud connection or remote authentication service to operate.

The application must store download history and settings locally.

## 2.5 Privacy by design

Quorix must not contain advertisements or third-party trackers.

Usage analytics must be disabled by default. Any future telemetry must be optional, disclosed and designed to avoid collecting download URLs or credentials.

## 2.6 Open development

Core functionality must remain publicly inspectable.

Source code, build instructions, licensing information, architecture documentation and benchmark methodology must be published.

---

# 3. Scope and Release Boundaries

## 3.1 Version 0.1: Transfer engine prototype

Required functionality:

- HTTP and HTTPS transfers.
- HTTP/1.1 and HTTP/2 where supported.
- Single-connection downloading.
- Segmented downloading.
- Pause and resume.
- Persistent progress metadata.
- Error reporting.
- Basic checksum verification.
- Command-line control.
- Automated transfer tests.

No desktop interface is required at this stage.

## 3.2 Version 0.5: Desktop alpha

Add:

- Windows desktop interface.
- Multiple simultaneous downloads.
- Queue management.
- Download history.
- Default download directories.
- Categories.
- Speed limiting.
- Notifications.
- Manual URL input.
- Crash recovery.

## 3.3 Version 1.0: Stable public release

Required:

- Reliable HTTP/HTTPS engine.
- Adaptive segmentation.
- Tested crash recovery.
- Windows desktop interface.
- Chrome and Edge browser integration.
- Secure browser-to-desktop communication.
- Download scheduling.
- Concurrent download control.
- Proxy configuration.
- Automatic and manual speed controls.
- Download integrity checks.
- Clear error diagnosis.
- Accessible minimal interface.
- Signed installer and update verification.

## 3.4 Later releases

Planned enhancements:

- Firefox integration.
- Linux and macOS desktop applications.
- HLS and DASH media support.
- Optional integration with yt-dlp.
- BitTorrent and magnet downloads.
- Metalink and multi-mirror downloading.
- Optional HTTP/3 support.
- Remote API.
- Mobile clients.
- Optional device-to-device queue synchronization.

These features must not delay the first stable release unless the core milestones are complete.

---

# 4. System Architecture

## 4.1 Required architecture

Quorix must use a modular, engine-first architecture.

The download engine must not depend on the desktop user interface.

The application must have these layers:

**Layer 1: User Interfaces**

Desktop application, command-line interface and browser integration.

**Layer 2: Application Controller**

Receives commands, validates inputs, manages task lifecycle and coordinates the engine.

**Layer 3: Transfer Engine**

Performs network communication, scheduling, segmentation, retries and data validation.

**Layer 4: Persistence and Storage**

Stores download state, writes file data and supports recovery.

**Layer 5: Operating System Integration**

Handles files, credentials, networking, notifications, process lifecycle and platform-specific functions.

## 4.2 Suggested technology stack

| Component | Technology |
|---|---|
| Core language | Rust |
| Asynchronous runtime | Tokio |
| HTTP client | reqwest and hyper |
| TLS | rustls |
| Local metadata | SQLite |
| Serialization | serde |
| Diagnostics | tracing |
| Desktop framework | Tauri 2 |
| Frontend | React with TypeScript |
| Build system | Cargo, Vite |
| Browser extension | WebExtensions |
| Browser communication | Native Messaging |
| Integrity checks | SHA-256 and BLAKE3 |
| Media processing | FFmpeg integration |
| CI/CD | GitHub Actions |
| Test environment | Local HTTP test server and fault injection |

The implementation must pin tested dependency versions rather than automatically selecting every new release.

## 4.3 Repository structure

Use a Rust workspace with independently testable modules.

```text
quorix/
├── apps/
│   ├── desktop/
│   └── cli/
├── crates/
│   ├── quorix-core/
│   ├── quorix-http/
│   ├── quorix-scheduler/
│   ├── quorix-storage/
│   ├── quorix-state/
│   ├── quorix-integrity/
│   ├── quorix-security/
│   ├── quorix-ipc/
│   └── quorix-media/
├── extensions/
│   ├── chromium/
│   └── firefox/
├── tests/
│   ├── integration/
│   ├── fault-injection/
│   ├── performance/
│   └── compatibility/
├── benchmarks/
├── docs/
├── scripts/
├── .github/
└── Cargo.toml
```

Every module must expose documented interfaces.

Avoid circular dependencies.

---

# 5. Download Engine Requirements

## 5.1 Download initialization

When a new download is created, the engine must:

1. Validate the URL and supported scheme.
2. Apply the configured proxy and authentication settings.
3. Follow allowed redirects under a bounded redirect policy.
4. Inspect response metadata.
5. Determine the resource size when available.
6. Determine whether byte-range transfers can work.
7. Record available resource validators such as ETag.
8. Select the appropriate transfer strategy.
9. Create persistent task metadata.
10. Begin downloading.

The engine must handle servers that reject HEAD requests.

It must be possible to determine range capability using a controlled GET request.

An `Accept-Ranges` header is useful but must not be treated as proof that every range request will succeed.

## 5.2 Transfer modes

The engine must provide three modes.

### Mode A: Sequential transfer

Use one transfer when the server does not support suitable range requests or when the file is too small for segmentation to provide value.

### Mode B: Fixed segmented transfer

Divide the resource into planned byte intervals and transfer them concurrently.

### Mode C: Adaptive segmented transfer

Adjust pending work and connection concurrency based on actual transfer performance and server responses.

Adaptive mode must become the default after adequate testing.

## 5.3 Segment model

Each segment must contain:

- Segment identifier.
- Starting byte offset.
- Exclusive ending byte offset.
- Confirmed downloaded intervals.
- Current state.
- Assigned worker.
- Retry count.
- Last error.
- Measured throughput.

Use half-open intervals: `[start, end)`.

The engine must prevent overlapping ownership of writable byte intervals unless explicitly using a controlled redundant-fetch strategy.

## 5.4 Dynamic work distribution

The scheduler must support redistribution of incomplete work.

If one worker finishes while another has a large unfinished range, the scheduler may subdivide the remaining work.

It must only split bytes that have not already been committed as complete.

Segment size must be bounded so that excessive subdivision does not create unnecessary requests or metadata overhead.

## 5.5 Adaptive concurrency

The scheduler must consider:

- Average throughput.
- Recent throughput.
- Transfer latency.
- Connection establishment cost.
- Error rate.
- HTTP 429 and 503 responses.
- Server-advertised retry guidance.
- Remaining file size.
- Available memory.
- Disk write capacity.
- User-defined connection limits.

Use a conservative starting connection count.

Increase concurrency only when measurements indicate a useful gain.

Reduce concurrency after relevant errors, sustained throughput degradation or resource pressure.

The scheduler must not intentionally bypass server rate limits.

## 5.6 HTTP correctness

Implement handling for:

- `200 OK`
- `206 Partial Content`
- `301`, `302`, `303`, `307`, `308` redirects as appropriate.
- `401 Unauthorized`
- `403 Forbidden`
- `404 Not Found`
- `408 Request Timeout`
- `416 Range Not Satisfiable`
- `429 Too Many Requests`
- `500`, `502`, `503`, `504` server errors.

On a ranged request, verify the actual response status and Content-Range values before accepting bytes.

Do not append a complete `200 OK` response to a partial file when the client expected a range response.

Use identity content encoding for byte-range transfer where appropriate. Representation transformations and content codings must be handled without mixing incompatible offsets.

## 5.7 Connection reuse

The HTTP transport must support pooling and reuse where the protocol and server allow it.

Use HTTP/2 multiplexing when available.

Do not assume that the optimal number of TCP connections is equal to the optimal number of concurrent HTTP requests.

## 5.8 Retry policy

A retryable error must not trigger unlimited retries.

Implement:

- Bounded retries.
- Exponential backoff.
- Configurable maximum delay.
- Optional randomized jitter.
- Respect for valid Retry-After guidance.
- Distinction between permanent and temporary errors.
- Cancellation support during backoff.

A non-retryable authentication or authorization error must produce a clear message.

---

# 6. Reliable Pause, Resume and Recovery

This is a primary product requirement.

## 6.1 Persistent download state

The engine must save:

- Task identifier.
- Original and resolved URLs, with sensitive values protected.
- Destination path.
- Temporary file path.
- Expected resource size.
- Entity validators.
- Confirmed byte intervals.
- Segment progress.
- Transfer configuration.
- State and timestamps.
- Retry history.
- Final verification status.

The system must not store credentials or secret-bearing URLs in unprotected diagnostic logs.

## 6.2 Resume validation

Before resuming, Quorix must verify that existing partial data is still compatible with the remote resource.

Where possible, use strong ETags and appropriate conditional range requests.

A weak ETag must not be treated as a strong byte-for-byte validator.

If no suitable validator exists, apply a documented conservative recovery policy.

Never silently combine content from different resource versions.

## 6.3 Crash-safe storage

The metadata database must use transactions.

A stored checkpoint must never claim that bytes are safely complete when they have not been committed to durable storage.

Implement an ordering policy between file writes, required synchronization and checkpoint commits.

On restart, verify consistency before trusting saved progress.

## 6.4 Recovery behaviour

After an unexpected application termination:

1. Load unfinished tasks.
2. Validate local metadata and temporary files.
3. Recover confirmed intervals.
4. Revalidate the remote resource when necessary.
5. Resume only compatible data.
6. Notify the user if a safe resume is not possible.

No completed interval may be assumed valid solely because the earlier interface displayed it.

## 6.5 Failure rules

- A failed worker must not destroy valid data from other workers.
- A missing temporary file must produce a recoverable task error.
- A changed remote resource must trigger safe restart or user action.
- A disconnected destination drive must suspend affected tasks safely.
- A full disk must pause writing and report the actual problem.
- A failed recovery must never be reported as successful.

---

# 7. Storage and File Integrity

## 7.1 File writing

Use bounded buffers and positional file writes.

Avoid loading complete files into memory.

Do not allocate a buffer proportional to total file size.

Different workers must not write to the same byte interval concurrently.

## 7.2 Temporary files

Unfinished downloads must use a controlled temporary file format.

Do not overwrite an existing completed file without explicit user approval or a configured overwrite policy.

Use safe path handling and avoid directory traversal.

## 7.3 Completion

A task may enter the Completed state only after:

1. Every required byte interval is present.
2. Transfer completeness checks pass.
3. Available expected checksums are verified.
4. Required file data is committed.
5. Finalization succeeds.
6. The output is available at the expected destination.

A failed rename or finalization operation must not produce a Completed state.

## 7.4 Hash verification

Provide SHA-256 for interoperability and BLAKE3 for efficient local hashing.

When the user supplies an expected hash, verification must compare against that expected value.

A locally calculated hash without a trusted reference is useful for future comparison but does not independently prove authenticity.

---

# 8. Task Lifecycle and State Machine

Every download must follow a defined state machine.

Required states:

- Created
- Inspecting
- Queued
- Downloading
- Pausing
- Paused
- Retrying
- Verifying
- Finalizing
- Completed
- Failed
- Cancelled

Only approved state transitions are permitted.

Example:

Created → Inspecting → Queued → Downloading → Verifying → Finalizing → Completed.

A transfer error may move Downloading to Retrying or Failed.

Pause must move through Pausing before Paused, so that active workers can stop safely.

Cancellation must be idempotent.

Repeating the same pause, resume or cancel command must not corrupt state.

---

# 9. Browser Integration

## 9.1 Purpose

Users must be able to hand downloads from supported browsers to Quorix with minimal effort.

## 9.2 Initial support

Version 1.0:

- Google Chrome.
- Microsoft Edge.
- Other compatible Chromium browsers when tested.

Firefox support is a separate compatibility target.

## 9.3 Required features

- Download interception where supported.
- Manual "Download with Quorix" context menu.
- Domain exclusions.
- File-type filters.
- Download confirmation settings.
- Per-site capture controls.
- Browser session context transfer when authorized and necessary.
- Clear indication when native integration is unavailable.

## 9.4 Communication architecture

Use browser Native Messaging with a separately installed native host.

Do not expose an unauthenticated HTTP endpoint that allows arbitrary file creation.

The native host must validate:

- Message schema.
- Supported command.
- Allowed extension identity.
- URL scheme.
- Requested destination.
- Message length.
- User authorization requirements.

The extension must not permit an arbitrary website to issue privileged native commands.

## 9.5 Handoff protocol

Every browser handoff must have a unique transaction identifier.

Suggested sequence:

1. Browser identifies an eligible download.
2. Extension sends a handoff request.
3. Native host validates the request.
4. Quorix acknowledges acceptance.
5. Browser and desktop coordinate ownership.
6. Quorix starts the transfer.
7. Both components record the final handoff result.

The design must minimize duplicate downloads.

If the handoff fails, the browser should retain or recover its normal download path where the browser APIs permit it.

Quorix must not claim to offer perfect atomic transfer ownership across browser and native processes.

---

# 10. Media Downloading

Media features must use separate modules.

## 10.1 Supported methods

Later versions may support:

- Direct MP4 and other supported media files.
- HLS playlists.
- MPEG-DASH manifests.
- Separate audio/video tracks.
- Media quality selection.
- Stream segment downloading.
- Remuxing and format validation.
- Optional yt-dlp integration.

## 10.2 Architecture

Use a media extraction layer separate from the general HTTP engine.

The media layer identifies resources and produces a download plan.

The transfer engine retrieves the required resources.

The assembly layer validates and combines compatible media streams.

## 10.3 Media verification

Before completion, validate:

- Required segment presence.
- Segment order.
- Output container structure.
- Track availability.
- Available duration and codec metadata.

Do not report completion when mandatory media assembly fails.

## 10.4 Content restrictions

Do not implement DRM circumvention as a product feature.

Respect copyright, user authorization and service restrictions.

Only support downloads that the user is permitted to make.

---

# 11. Desktop Interface Design

## 11.1 Design philosophy

Quorix must look simple even when its engine is complex.

The interface must not imitate IDM screen by screen.

It should provide its own consistent visual system.

## 11.2 Main screen

Required areas:

- Active downloads.
- Completed downloads.
- Queued downloads.
- Search and filters.
- Add download control.
- Settings.
- Download details.

## 11.3 Download item

Display:

- File icon and name.
- Progress bar.
- Completed and total size.
- Current speed.
- Remaining time.
- Current state.
- Main action buttons.

Do not show every connection and segment by default.

Provide an advanced details view for technical users.

## 11.4 Advanced details

Include:

- Source URL, with sensitive parameters concealed when displayed.
- HTTP status.
- Connection count.
- Transfer mode.
- Segment progress.
- Retry count.
- Recent throughput.
- Error diagnosis.
- Resource validators.
- Hash verification state.

## 11.5 User experience requirements

- Common tasks must be available in one or two direct actions.
- No mandatory account creation.
- No advertisements.
- No distracting animations.
- Consistent keyboard shortcuts.
- Clear error messages.
- Accessible labels.
- High-contrast support.
- Light and dark themes.
- Responsive layout for supported screen sizes.

## 11.6 Settings

Organize settings into:

- General.
- Downloads.
- Network.
- Browser integration.
- Scheduling.
- Privacy and security.
- Advanced.
- About.

Advanced configuration must not appear in the initial setup unless it is necessary.

---

# 12. Queue and Scheduling System

The queue must support:

- Multiple queued tasks.
- User-defined priorities.
- Maximum active downloads.
- Per-host concurrency limits.
- Scheduled start and stop.
- Automatic retries.
- Pause after current task.
- Optional shutdown after completion.
- Category-based download paths.

Prevent one large transfer from permanently starving smaller queued tasks.

Queue scheduling must consider both priority and fairness.

A download scheduled to stop must checkpoint progress before shutting down its workers.

---

# 13. Security Requirements

## 13.1 Trust boundaries

Treat downloaded data, remote metadata, browser messages, media manifests and imported task files as untrusted input.

## 13.2 Required controls

- TLS certificate validation.
- Safe redirect handling.
- URL scheme restrictions.
- Path canonicalization.
- File overwrite protection.
- Strict IPC message validation.
- Secure credential storage.
- No arbitrary command execution through download metadata.
- Controlled temporary directory permissions.
- Signed software updates.
- Dependency vulnerability checks.
- Secrets redaction from logs.

## 13.3 Local services

Any optional local RPC service must be disabled by default unless required by a controlled component.

If enabled, it must use authentication, explicit binding rules and appropriate origin protections.

## 13.4 Security testing

Test:

- Malformed HTTP responses.
- Invalid Content-Range values.
- Path traversal attempts.
- Malicious filenames.
- Oversized IPC messages.
- Invalid extension identities.
- Unexpected local API requests.
- Redirects to restricted resources.
- Corrupt resume metadata.
- Invalid media manifests.

Security findings must block release when they create a credible high-severity risk.

---

# 14. Performance Engineering

## 14.1 Performance objective

Quorix should use available network bandwidth efficiently without causing excessive system load or ignoring server limits.

Do not target an arbitrary multiplier such as 10× faster than IDM.

Instead, compare transfer efficiency under controlled conditions.

## 14.2 Key measurements

Record:

- Total completion time.
- Useful payload throughput.
- Connection count.
- Retry count.
- Failed requests.
- Duplicate bytes transferred.
- CPU utilization.
- Peak resident memory.
- Disk throughput.
- Transfer completion rate.
- Integrity failure count.
- Resume success rate.

## 14.3 Baseline competitors

Use:

- IDM.
- aria2.
- Gopeed.
- AB Download Manager.
- Motrix.

Select comparable versions, configurations and connection counts.

## 14.4 Test environments

Test:

1. Low-bandwidth connection.
2. High-bandwidth connection.
3. High latency.
4. Packet loss.
5. Server-side throttling.
6. Connection rejection.
7. Disk write bottleneck.
8. Large files.
9. Very small files.
10. Application restart and network interruption.

Do not use uncontrolled public-server comparisons as the principal performance evidence.

## 14.5 Proposed acceptance targets

Targets below are engineering goals that must be tested.

| Metric | Target |
|---|---|
| Integrity | Zero incorrect files reported as verified in the release test suite |
| Crash recovery | 100% correct recovery decisions across defined controlled scenarios |
| HTTP compatibility | All mandatory protocol tests pass |
| Peak transfer memory | Under 150 MB for the core transfer process in a defined 10 GB / 8-worker benchmark |
| Network efficiency | At least 90% of the calibrated reference throughput in suitable local test conditions |
| Idle desktop CPU | Under 1% average on the specified reference machine |
| Download failure diagnosis | Every tested failure maps to a defined error code |
| Installer verification | 100% of release artifacts pass signature/hash checks |

These targets must include the hardware specification, test configuration and repeated-run method in the benchmark report.

A failure to meet a target requires investigation, not manipulation of the benchmark.

---

# 15. Testing and Quality Assurance

## 15.1 Test layers

Maintain:

- Unit tests.
- Integration tests.
- Protocol compatibility tests.
- Recovery tests.
- Security tests.
- Performance tests.
- User interface tests.
- Installation and upgrade tests.

## 15.2 Mandatory failure scenarios

The automated suite must test:

1. Server ignores Range and returns HTTP 200.
2. Server sends incorrect Content-Range.
3. Server sends HTTP 416.
4. Server returns HTTP 403 for additional connections.
5. Server returns HTTP 429 with Retry-After.
6. Connection closes mid-transfer.
7. Remote resource changes before resume.
8. ETag changes.
9. Expected content length is wrong.
10. Disk becomes full.
11. Destination drive disconnects.
12. Process terminates during a file write.
13. Process terminates during a database checkpoint.
14. Partial file is modified externally.
15. Download URL expires.
16. Proxy authentication fails.
17. TLS certificate validation fails.
18. File checksum does not match.
19. Browser extension loses contact with the native host.
20. Multiple concurrent tasks are cancelled.

## 15.3 Test evidence

Each test must include:

- Test identifier.
- Preconditions.
- Input data.
- Expected result.
- Actual result.
- Pass or fail.
- Relevant logs.
- Software version.

Critical regressions must receive permanent automated tests.

---

# 16. Development Procedure

## Phase 0: Discovery and technical specification

Tasks:

- Review competing implementations.
- Analyze existing downloader issue reports.
- Record mandatory requirements.
- Define supported operating systems.
- Select dependencies.
- Build a threat model.
- Define benchmark workloads.
- Create the initial issue backlog.

Deliverable: Approved product requirements and architecture.

Exit condition: No unresolved critical architecture decisions.

## Phase 1: Minimal HTTP engine

Tasks:

- Create Rust workspace.
- Implement sequential HTTP downloading.
- Add redirects and response validation.
- Add cancellation.
- Add basic file writing.
- Add unit and integration tests.

Deliverable: Working command-line downloader.

Exit condition: Correct transfers against the local HTTP test server.

## Phase 2: Segmentation

Tasks:

- Add byte-range inspection.
- Implement interval tracking.
- Add concurrent workers.
- Validate partial responses.
- Add segment progress events.
- Test overlaps, gaps and server fallbacks.

Deliverable: Correct segmented engine.

Exit condition: Complete data coverage without incorrect overlapping writes.

## Phase 3: Resume and persistence

Tasks:

- Introduce SQLite metadata.
- Implement persistent progress.
- Add source validation.
- Add crash recovery.
- Handle changed resources.
- Handle missing or corrupt temporary files.

Deliverable: Crash-recoverable transfer system.

Exit condition: All mandatory recovery tests pass.

## Phase 4: Adaptive performance

Tasks:

- Measure throughput per connection.
- Implement concurrency control.
- Add adaptive work splitting.
- Implement retries and backoff.
- Add bandwidth limiting.
- Add performance instrumentation.

Deliverable: Adaptive transfer engine.

Exit condition: Demonstrated improvement or equivalent throughput without worse reliability in controlled tests.

## Phase 5: Desktop application

Tasks:

- Integrate Tauri.
- Build download list.
- Add queue controls.
- Add settings and categories.
- Implement native notifications.
- Add keyboard navigation and accessibility.
- Add light and dark themes.

Deliverable: Windows desktop alpha.

Exit condition: Core workflows pass user interface tests.

## Phase 6: Browser integration

Tasks:

- Build Chromium extension.
- Implement native host registration.
- Build validated IPC protocol.
- Add context menu and capture rules.
- Handle handoff failures.
- Add per-site controls.

Deliverable: Browser-integrated beta.

Exit condition: No unresolved high-severity handoff or native-host security issues.

## Phase 7: Stabilization

Tasks:

- Fix critical and high-severity bugs.
- Run full regression tests.
- Conduct controlled benchmarks.
- Audit dependencies.
- Review installers and updater.
- Test clean installation and upgrade.
- Prepare user documentation.

Deliverable: Release candidate.

Exit condition: All release gates pass.

## Phase 8: Public release

Tasks:

- Publish tagged source code.
- Publish signed installers.
- Publish checksums.
- Publish release notes.
- Publish known limitations.
- Publish benchmark methods and results.
- Enable issue reporting.
- Maintain a security contact.

Deliverable: Quorix 1.0.

---

# 17. Engineering Workflow and Governance

## 17.1 Issue management

Every development task must have a tracked issue.

Classify issues as:

- Bug.
- Feature.
- Performance.
- Security.
- Documentation.
- Maintenance.

Add severity and affected module where applicable.

## 17.2 Pull requests

Every significant code change must include:

- Problem statement.
- Implementation summary.
- Test evidence.
- Risk assessment.
- Documentation changes when needed.

Core engine changes must receive code review before merging.

## 17.3 Coding standards

- Use rustfmt.
- Use Clippy.
- Avoid unnecessary unsafe Rust.
- Document unsafe code invariants when unsafe code is necessary.
- Handle errors explicitly.
- Avoid unbounded task spawning.
- Avoid shared mutable state without defined synchronization.
- Write deterministic tests wherever possible.

## 17.4 Dependency management

- Use a lockfile for reproducible applications.
- Review major dependency upgrades.
- Run automated vulnerability audits.
- Publish dependency and licence information.
- Do not automatically execute newly downloaded helper binaries without integrity and source verification.

---

# 18. Open-Source Licensing

Select the licence before public contribution begins.

Recommended options:

**GPL-3.0-or-later:** Suitable if the objective is to require distributed derivative works to provide corresponding source under compatible copyleft terms.

**Apache-2.0 or MIT:** Suitable for broad reuse of the engine in other software, including proprietary products, subject to the relevant licence terms.

The choice must reflect whether broad library adoption or strong copyleft protection is the higher priority.

Review third-party dependencies and media-component redistribution requirements before release.

Do not assume that every library or binary can be redistributed under the same terms.

---

# 19. Documentation Requirements

The public repository must contain:

- README.
- Installation instructions.
- Architecture overview.
- Supported protocols.
- Feature matrix.
- Known limitations.
- Build instructions.
- Contributor guide.
- Code of conduct.
- Security policy.
- Licence.
- API documentation.
- Troubleshooting guide.
- Benchmark methodology.
- Release notes.

The README must state what Quorix can do today, not only what the project intends to support.

---

# 20. Definition of Done

A feature is complete only when:

1. Its behaviour matches the approved requirement.
2. Its error paths are handled.
3. Automated tests are present.
4. Required documentation is updated.
5. Performance impact is evaluated where relevant.
6. Security implications are reviewed.
7. No known critical regression is introduced.
8. The change passes code review.

A release is complete only when the defined release gates pass.

---

# 21. Explicit Non-Goals

Quorix must not attempt to deliver every possible feature in version 1.0.

The following are not initial goals:

- Cloud storage service.
- Social platform.
- Mandatory online account.
- Advertising network.
- DRM circumvention.
- Universal website scraping.
- AI-powered features without demonstrated value.
- Proprietary server infrastructure required for basic downloads.
- Unverified claims of being the world's fastest downloader.

The core product must remain focused on downloading files reliably.

---

# 22. Project Success Criteria

Quorix succeeds when:

- Users can start downloads without technical knowledge.
- Interrupted transfers recover safely when the source permits recovery.
- The application handles incompatible servers predictably.
- Its transfer engine uses bandwidth efficiently.
- Large files do not create unbounded memory consumption.
- Browser integration does not silently lose download requests.
- Completed files meet integrity requirements.
- Users can understand failures without reading raw logs.
- The software remains fast, minimal and maintainable.
- Other developers can reuse Quorix Core.

Popularity, download counts and GitHub stars may indicate adoption, but they must not replace engineering-quality measurements.

---

# 23. Final Engineering Directive

**Build the download engine first. Prove its reliability. Measure its performance. Then build the interface and integrations around it.**

Do not prioritize appearance over transfer correctness.

Do not prioritize connection count over useful throughput.

Do not prioritize feature quantity over reliability.

Quorix must earn its reputation through reproducible performance, trustworthy recovery and a simple user experience.

**End of SOP — Version 1.0**
