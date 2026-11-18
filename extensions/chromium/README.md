# Quorix Connect (Chromium)

Browser extension that hands downloads from Chrome and Edge to the Quorix desktop app.

## Status

Early code. The desktop app and its native messaging host do not exist yet, so the extension always reports **Not connected** and every download stays in the browser. That is the designed fallback, not an error.

What works today:

- Popup with a link field, connection status and a list of recent handoffs.
- "Download with Quorix" in the context menu for links, video, audio and images. When Quorix is unavailable, the link is downloaded by the browser instead.
- Options page: capture switch, file types, minimum size, excluded sites.
- Automatic capture logic (off by default). It can only be exercised once a native host answers.

Not built yet:

- Native messaging host (`com.quorix.connect`) and its installer registration.
- Passing browser session context (cookies, headers) for downloads that need it.
- Video and audio from a page. The protocol reserves `kind: "media"` for this.

## Load it for development

1. Run `node scripts/sync-theme.mjs` from the repository root. This copies `theme/` into `extensions/chromium/theme/`.
2. Open `chrome://extensions` (or `edge://extensions`) and turn on **Developer mode**.
3. Choose **Load unpacked** and select `extensions/chromium`.

## Safety rules

These come from section 9 of `Quorix_SOP.md`.

- The browser download is paused, not cancelled, while Quorix decides. It is cancelled only after Quorix confirms ownership. On any failure it is resumed.
- Only `http` and `https` URLs up to 8192 characters are sent.
- Replies are checked for protocol version, matching transaction id and shape before they are trusted.
- The service worker accepts messages only from this extension's own pages. Web pages cannot send commands.
- No remote code, no remote fonts, no analytics. The only peer is the native host on the same computer.

## Protocol (version 1)

Defined in `protocol.js`.

```text
-> { v: 1, id, cmd: "ping" }
<- { v: 1, id, ok: true, version }

-> { v: 1, id, cmd: "handoff", url, kind: "file", source, referrer?, filename? }
<- { v: 1, id, ok: true, task_id }
<- { v: 1, id, ok: false, code, message }
```

`source` is `popup`, `context-menu` or `capture`. `id` is a UUID that names one handoff on both sides.

## Permissions

| Permission | Why |
|---|---|
| `nativeMessaging` | Talk to the Quorix app on this computer |
| `contextMenus` | "Download with Quorix" menu item |
| `downloads` | Pause, resume or cancel a browser download during a handoff |
| `storage` | Remember options |

No host permissions are requested.

## Licence

Quorix Connect has the licence GPL-3.0-or-later. Read [LICENSE](../../LICENSE).
