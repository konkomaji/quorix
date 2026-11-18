// SPDX-License-Identifier: GPL-3.0-or-later
// Quorix Connect service worker.
// Rule that shapes this file: a download is never taken away from the browser
// until the desktop app has confirmed it owns it. If anything fails, the
// browser keeps or resumes its own download.

import {
  PROTOCOL_VERSION, HOST_NAME, HOST_TIMEOUT_MS, REASONS,
  loadSettings, validUrl, displayName
} from './protocol.js';

const MENU_ID = 'quorix-download';
const RECENT_LIMIT = 20;

// ---------- native host ----------

/** Sends one message to the native host. Always resolves to a reply object. */
function askHost(message) {
  const id = crypto.randomUUID();
  return new Promise(resolve => {
    const timer = setTimeout(() => resolve({ ok: false, code: 'timeout', message: REASONS.timeout }), HOST_TIMEOUT_MS);
    const finish = reply => { clearTimeout(timer); resolve(reply); };
    try {
      chrome.runtime.sendNativeMessage(HOST_NAME, { v: PROTOCOL_VERSION, id, ...message }, reply => {
        if (chrome.runtime.lastError || !reply) {
          return finish({ ok: false, code: 'no-host', message: REASONS['no-host'] });
        }
        if (reply.v !== PROTOCOL_VERSION || reply.id !== id || typeof reply.ok !== 'boolean') {
          return finish({ ok: false, code: 'bad-reply', message: REASONS['bad-reply'] });
        }
        if (!reply.ok) {
          return finish({ ok: false, code: String(reply.code || 'rejected'), message: String(reply.message || REASONS.rejected).slice(0, 300) });
        }
        finish(reply);
      });
    } catch {
      finish({ ok: false, code: 'no-host', message: REASONS['no-host'] });
    }
  });
}

async function hostStatus() {
  const reply = await askHost({ cmd: 'ping' });
  return reply.ok ? { connected: true, version: String(reply.version || '') } : { connected: false, reason: reply.message };
}

// ---------- handoff ----------

async function remember(entry) {
  const { recent = [] } = await chrome.storage.session.get('recent');
  recent.unshift(entry);
  await chrome.storage.session.set({ recent: recent.slice(0, RECENT_LIMIT) });
}

/** Asks the desktop app to take a download. Records the result either way. */
async function handoff({ url, source, referrer, filename, kind = 'file' }) {
  const safe = validUrl(url);
  const reply = safe
    ? await askHost({ cmd: 'handoff', url: safe, kind, source, referrer: validUrl(referrer) || undefined, filename })
    : { ok: false, code: 'bad-url', message: REASONS['bad-url'] };

  await remember({
    name: filename || displayName(safe || String(url)),
    ok: reply.ok,
    message: reply.ok ? 'Sent to Quorix' : reply.message,
    source,
    at: Date.now()
  });
  return reply;
}

// ---------- context menu ----------

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: MENU_ID, title: 'Download with Quorix', contexts: ['link', 'video', 'audio', 'image'] });
  });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId !== MENU_ID) return;
  const reply = await handoff({ url: info.linkUrl || info.srcUrl, source: 'context-menu', referrer: info.pageUrl || tab?.url });
  // The user asked for Quorix explicitly, so fall back to a normal browser download when it is unavailable.
  if (!reply.ok && reply.code !== 'bad-url') {
    const url = validUrl(info.linkUrl || info.srcUrl);
    if (url) { ourDownloads.add(url); chrome.downloads.download({ url }).catch(() => ourDownloads.delete(url)); }
  }
});

// ---------- automatic capture (off by default) ----------

const handingOff = new Set(); // browser download ids currently being offered to the desktop app
const ourDownloads = new Set(); // urls this extension started itself as a fallback

function wanted(item, settings) {
  const url = validUrl(item.finalUrl || item.url);
  if (!url) return false;
  const host = new URL(url).hostname;
  const excluded = settings.excludedDomains.split(/\s+/).filter(Boolean);
  if (excluded.some(d => host === d || host.endsWith('.' + d))) return false;
  if (item.totalBytes > 0 && item.totalBytes < settings.minSizeMb * 1024 * 1024) return false;
  const ext = (item.filename || new URL(url).pathname).split('.').pop().toLowerCase();
  return settings.fileTypes.split(/[\s,]+/).filter(Boolean).includes(ext);
}

chrome.downloads.onCreated.addListener(async item => {
  if (item.state !== 'in_progress' || handingOff.has(item.id)) return;
  if (ourDownloads.delete(item.url)) return; // a fallback this extension started: never offer it back
  const settings = await loadSettings();
  if (!settings.capture || !wanted(item, settings)) return;

  handingOff.add(item.id);
  try {
    // Hold the browser download while the desktop app decides. If it cannot be paused, leave it alone.
    try { await chrome.downloads.pause(item.id); } catch { return; }

    const reply = await handoff({ url: item.finalUrl || item.url, source: 'capture', referrer: item.referrer });
    if (reply.ok) {
      await chrome.downloads.cancel(item.id).catch(() => {});
      await chrome.downloads.erase({ id: item.id }).catch(() => {});
    } else {
      await chrome.downloads.resume(item.id).catch(() => {});
    }
  } finally {
    handingOff.delete(item.id);
  }
});

// ---------- messages from the popup and options page ----------

chrome.runtime.onMessage.addListener((msg, sender, respond) => {
  if (sender.id !== chrome.runtime.id) return false; // only this extension's own pages
  (async () => {
    switch (msg?.type) {
      case 'status': return respond(await hostStatus());
      case 'handoff': return respond(await handoff({ url: msg.url, source: 'popup' }));
      case 'recent': return respond((await chrome.storage.session.get('recent')).recent || []);
      default: return respond({ ok: false, code: 'bad-request', message: 'Unknown request' });
    }
  })();
  return true; // keep the channel open for the async reply
});
