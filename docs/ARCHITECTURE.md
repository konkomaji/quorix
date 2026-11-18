# Architecture

This file describes the structure of Quorix as it is today.
The requirements are in [Quorix_SOP.md](../Quorix_SOP.md).

## Layers

```text
+----------------------------------------------------------+
|  Interfaces      apps/cli        extensions/chromium     |
+----------------------------------------------------------+
|  Engine          quorix-core                             |
|                  state machine, errors, transfer modes   |
+------------------+-------------------+-------------------+
|  quorix-http     |  quorix-storage   |  quorix-state     |
|  requests,       |  temporary file,  |  byte intervals,  |
|  redirects       |  final move       |  progress file    |
+------------------+-------------------+-------------------+
|  quorix-integrity          SHA-256 and BLAKE3            |
+----------------------------------------------------------+
```

Rules:

- An interface uses the engine. The engine does not use an interface.
- All interfaces use the same engine. There is one implementation of a download.
- Only `quorix-http` uses the HTTP library. Other crates do not see its types.

## States of a download

A download has one of 12 states.
The engine permits only the changes in this table.

| From | To |
|---|---|
| Created | Inspecting, Failed, Cancelled |
| Inspecting | Queued, Failed, Cancelled |
| Queued | Downloading, Paused, Failed, Cancelled |
| Downloading | Pausing, Retrying, Verifying, Failed, Cancelled |
| Pausing | Paused, Failed, Cancelled |
| Paused | Queued, Cancelled |
| Retrying | Downloading, Pausing, Failed, Cancelled |
| Verifying | Finalizing, Failed, Cancelled |
| Finalizing | Completed, Failed |
| Failed | Queued |
| Completed | none |
| Cancelled | none |

Important results of this table:

- Only Finalizing can change to Completed. No download skips verification.
- A pause goes through Pausing. The connections stop safely before the state is Paused.
- Finalizing cannot change to Cancelled. The final move occurs fully or it fails.

## Inspection

The engine does not trust the `Accept-Ranges` header.
It sends a range request for the first part of the file and examines the answer.

| Answer | Meaning | Action |
|---|---|---|
| `206` with a correct `Content-Range` | The server supports byte ranges. | Use many connections. The answer is the first part. |
| `200` | The server sent the full file. | Use one connection. The answer is the download. |
| `416` | The file is possibly empty. | Send a request without a range. |
| Other status | An error. | Try again if the error is temporary. |

No request is used only for inspection.

## Transfer with many connections

1. The engine divides the missing bytes into parts.
2. The parts go into a queue.
3. Each connection takes one part from the queue.
4. The connection sends a range request with `If-Range` and the strong `ETag`.
5. The connection examines the answer before it accepts data.
6. The connection writes the data at the correct position.
7. The connection takes the next part.

A slow connection holds a maximum of one part. The other connections continue.

### Rules that the engine keeps

- One part has one owner at a time. Two connections write the same bytes only in a hedged request. Read the section "Hedged requests".
- The engine accepts data only from a `206` answer with a `Content-Range` that agrees with the request.
- A connection does not write out of its part, whatever the server sends.
- If the server sends `200` to a request with `If-Range`, the file changed. The engine stops and deletes the data.

### When a server refuses connections

If a connection gets status 403, 429 or 503 while other connections operate, it gives its part back to the queue and stops.
The download continues with fewer connections.

## Adaptive number of connections

The engine does not assume how many connections are useful. It measures.

1. The transfer starts with 4 connections.
2. The engine measures the bytes that arrive in a window of 0.8 seconds.
3. The first window is ignored. Connections are still starting, and the speed reads low.
4. After a measured window, the engine makes the number of connections two times larger.
5. The window after a change is ignored. The new connections are still starting.
6. If the next measured speed is 15% higher or more, the engine continues at step 4.
7. If it is not, the engine goes back to the number before and stops the measurement.

The engine counts bytes when they arrive from the network.
It does not count them when they are written to the disk, because writes occur in large steps and make a short window inaccurate.

A refusal from the server (status 403, 429 or 503) stops the increase for the remaining download.

A fixed number of connections disables this mechanism.

## Hedged requests

A hedged request fetches bytes that a different connection is already fetching.

When a connection has no part to take, it examines the parts that other connections hold.
It requests the remaining bytes of a part again if one of these conditions is true:

- The owner of the part made no progress for 2 seconds.
- The owner is 4 times slower than this connection was on its last part, and the part is 0.5 seconds old or more.

The connection that writes the last byte marks the part as complete. The other connection stops.

Two connections then write the same bytes to the same positions.
This is safe only if the bytes are identical. Thus the engine uses a hedged request only when the server sent a strong `ETag`, and each request has `If-Range` with that `ETag`.
Without a strong `ETag`, no part is fetched two times.

The progress counter adds only bytes that were not on the disk before. A hedged request does not make the progress larger than the file.

## Progress file and recovery

The progress file is `<name>.qrxpart.state`.

It contains:

- An identity of the resource. This is a hash of the scheme, the host, the port and the path. The query string is not included.
- The size of the file.
- The strong `ETag`.
- The list of byte intervals that are on the disk.

- The block size, and the BLAKE3 hash of each completed block.

It does not contain the link.

### Block hashes

The engine divides the file into blocks of equal size. The last block can be smaller.
The block size depends only on the file size. Thus a later run calculates the same value.

| File size | Block size |
|---|---|
| Up to 256 MiB | 64 KiB |
| 1 GiB | 256 KiB |
| 50 GiB and larger | 16 MiB |

The number of blocks is approximately 4096 at most. This keeps the progress file small.

When a save occurs, the engine finds the blocks that are fully on the disk and have no hash.
It reads each such block from the file and calculates the hash.
The hash thus describes the content of the file. It does not describe what the engine sent to the file.

### Sequence of a save

1. Copy the list of written intervals.
2. Synchronize the temporary file to the disk.
3. Write the progress file to a new file and synchronize it.
4. Rename the new file over the old progress file.

The copy is made before the synchronization. Thus each interval in the progress file was on the disk before the progress file was written.

### Conditions for resume

The engine uses old data only if all these conditions are true:

- Resume is permitted by the caller.
- The server sent a strong `ETag`.
- The progress file exists and is consistent.
- The identity, the size and the `ETag` agree.
- The temporary file is a regular file with the correct size.

If one condition is false, the engine starts from zero.
A weak `ETag` is not accepted as proof.

### Check of saved data

The conditions above show that the server did not change the file.
They do not show that the temporary file on this computer is unchanged.
Thus the engine examines the saved data before it uses it:

1. Read each block that has a saved hash.
2. Calculate the BLAKE3 hash of the block.
3. If the hash agrees, keep the block.
4. If the hash does not agree, or the block cannot be read, download the block again.

Bytes that are not in a block with a saved hash are downloaded again.

The caller can disable this check. Then the engine uses all saved data as it is.

## Retries

- A counter holds the number of failures in sequence without new data.
- New data sets the counter to zero.
- The wait time is two times longer after each failure, up to a maximum. A random part prevents many clients from trying at the same moment.
- If the server sends `Retry-After`, the engine waits for that time. If the time is too long, the engine stops with the error from the server.
- A permanent error stops the download immediately.

## Storage

- The temporary file is `<name>.qrxpart` in the destination folder.
- On Windows the temporary file is marked as sparse. A write far into the file does not cause the system to fill the space before it with zeros.
- Writes use a position. The connections do not share a file cursor.
- The final move does not replace a file that appeared during the download, unless the user permitted it.

## Verification

1. The engine examines that all intervals are present.
2. The engine examines the size of the file on the disk.
3. If the user supplied a checksum, the engine reads the file from the disk and compares the checksum.

The checksum is calculated from the disk. It shows what the disk contains, not what the engine sent to the disk.

## Unsafe code

The workspace has the lint rule `unsafe_code = "deny"`.
There is one exception: the function `mark_sparse` in `quorix-storage`.
It makes one call to the Windows system. The safety conditions are written next to the call.
