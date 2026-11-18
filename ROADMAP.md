# Roadmap

This file lists the planned work for Quorix.
The versions are milestones. They are not dates.
A version is complete when its tests pass.

The engineering rules are in [Quorix_SOP.md](Quorix_SOP.md).

## Status of the versions

| Version | Content | Status |
|---|---|---|
| 0.1 | Transfer engine and command-line tool | In work |
| 0.5 | Desktop application for Windows, alpha | Not started |
| 1.0 | Stable release with browser integration | Not started |
| Later | Media, more platforms, more protocols, device transfer | Not started |

## Version 0.1: transfer engine

| Item | Status |
|---|---|
| HTTP and HTTPS transfer | Done |
| Transfer with one connection | Done |
| Transfer with many connections | Done |
| Pause and resume | Done |
| Progress file that is safe after a crash | Done |
| Retries with a limit and an increasing wait time | Done |
| SHA-256 and BLAKE3 verification | Done |
| Command-line tool | Done |
| Fault-injection tests | Done, more scenarios to add |
| Quorix Bench, first version | Done |

## Engine work after 0.1, in this sequence

The sequence is important. Each step makes the next step possible to measure.

### 1. Quorix Bench

A tool that measures speed and reliability in controlled conditions.

- It runs the same download many times and reports the results.
- It compares Quorix with other tools, for example aria2 and curl, with equal settings.
- It publishes the method, the hardware and the raw numbers.

Status: the first version is done. It uses a local server with three conditions.

Still to do:

- Add conditions: high latency, packet loss, a slow disk, very small files, a restart in the middle.
- Measure processor use and peak memory.
- Run the comparison with aria2 on a computer that has it.
- Publish results from more than one computer.

No speed statement about Quorix is permitted without a published Quorix Bench report that supports it.

### 2. Adaptive scheduler with hedged requests

- The engine measures the speed of each connection.
- The engine adds connections only while the total speed increases.
- The engine removes connections when errors or slow speed show that the server has a limit.
- Near the end of a download, the engine requests a slow part again on a fast connection. The first answer that is complete wins.

### 3. Hash tree for each part

- The engine calculates a BLAKE3 hash tree while the data arrives.
- The progress file stores the hashes of the completed parts.
- On resume, the engine examines each stored part against its hash.

Result: Quorix can prove that a resumed file is correct. A temporary file that a different program changed is found without a full checksum from the user.

### 4. Multipath transfer

- The engine uses two or more network adapters at the same time, for example Ethernet and Wi-Fi.
- Each connection is attached to one adapter.
- The total speed can be near the sum of the adapters.

### 5. Other engine items

- HTTP/3, as an option.
- Download from many mirrors at the same time.
- Speed limit.
- Proxy settings.
- A check of free disk space before the download starts.

## Version 0.5: desktop application, alpha

- Windows application made with Tauri 2 and React.
- Many downloads at the same time.
- Queue, history, categories and default folders.
- Notifications.
- SQLite store for the queue and the history.

The interface design is fixed. It uses the theme in `theme/`.

## Version 1.0: stable release

- Browser integration for Chrome and Edge with a native messaging host.
- Download schedule.
- Clear diagnosis for each error.
- Accessible interface with light and dark themes.
- Signed installer and verified updates.

## Later

### Media

- Video and audio from pages, only where the user has permission to save them.
- HLS and MPEG-DASH streams.
- Optional use of yt-dlp.

Quorix will not bypass copy protection.

### Platforms and protocols

- Firefox extension.
- Linux and macOS applications.
- BitTorrent and magnet links.
- Metalink.

### Quorix Link: transfer between your own devices

A transfer protocol for two devices that both run Quorix.

- It uses QUIC as the transport.
- It adds forward error correction. Lost packets are repaired from extra data, and the sender does not have to send them again.
- It is for lossy links, where TCP becomes slow.
- The two devices pair directly. No Quorix server is necessary.

This is a separate feature. It does not make downloads from web servers faster, because web servers use HTTP.

## Items that Quorix will not have

- A mandatory account.
- Advertisements.
- Trackers from other companies.
- A method to bypass copy protection.
- Speed statements that have no published test.
