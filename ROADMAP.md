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

Status: the first version is done.

- The engine starts with 4 connections and measures the total speed.
- The engine makes the number of connections two times larger while the speed increases by 15% or more.
- If the speed does not increase, the engine goes back to the number before.
- If the server refuses a connection, the engine stops the increase for that download.
- If a part is on a connection that is 4 times slower than a free connection, or that made no progress for 2 seconds, the free connection requests the remaining bytes again. The first complete answer wins.

Still to do:

- Measure again later in a long download, because network conditions change.
- Make the increase faster for short downloads.
- Take the number of HTTP/2 streams into account, not only the number of connections.
- Measure the behaviour on real networks with Quorix Bench.

### 3. Block hashes for a proven resume

Status: the first version is done.

- The engine divides the file into blocks. There are approximately 4096 blocks at most.
- When a block is complete and synchronized, the engine reads it from the disk and calculates a BLAKE3 hash.
- The progress file stores the hash of each completed block.
- On resume, the engine reads each saved block and compares the hash. Only blocks that agree are used again.

Result: a resumed download does not depend on the temporary file being unchanged. Quorix examines it.

Still to do:

- Join the block hashes into a full hash tree with one root value.
- Use the root value to compare a file with a trusted value from the source, when a source supplies one.
- Show the progress of the check for very large files.
- Calculate the hashes on more than one processor core.

### 4. Multipath transfer

Status: the first version is done.

- The user gives the local address of each network adapter.
- The engine makes one HTTP client for each address and divides the connections between them.
- If an adapter fails 3 times in sequence while a different adapter operates, the engine stops the use of that adapter and tells the user.
- If no adapter can connect, the download fails with a connection error.

Still to do:

- Find the network adapters automatically.
- Measure the speed on two real internet links. The current tests use two local addresses on one computer.
- Try an adapter again after some time, because a network can come back.
- Give more connections to the faster adapter.

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
