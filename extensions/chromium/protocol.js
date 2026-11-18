// SPDX-License-Identifier: GPL-3.0-or-later
// Quorix Connect <-> native host protocol, version 1.
// Shared by the service worker, the popup and the options page.
//
// Request  { v, id, cmd: "ping" }
// Request  { v, id, cmd: "handoff", url, kind, source, referrer?, filename? }
// Reply    { v, id, ok: true,  version?, task_id? }
// Reply    { v, id, ok: false, code, message }
//
// `kind` is "file" today. "media" is reserved for video and audio taken from a
// page (a later release); the host must reject kinds it does not know.

export const PROTOCOL_VERSION = 1;
export const HOST_NAME = 'com.quorix.connect';
export const MAX_URL_LENGTH = 8192;
export const HOST_TIMEOUT_MS = 5000;

export const DEFAULT_SETTINGS = Object.freeze({
  capture: false,                 // take over downloads the browser starts
  minSizeMb: 1,                   // leave smaller files to the browser
  fileTypes: 'zip 7z rar iso img exe msi dmg pkg deb rpm apk tar gz bz2 xz mp4 mkv mov webm mp3 flac wav pdf',
  excludedDomains: ''             // one host per line; never captured
});

export async function loadSettings() {
  return { ...DEFAULT_SETTINGS, ...(await chrome.storage.sync.get(DEFAULT_SETTINGS)) };
}

/** Returns the URL string when it is safe to hand off, otherwise null. */
export function validUrl(raw) {
  if (typeof raw !== 'string' || raw.length === 0 || raw.length > MAX_URL_LENGTH) return null;
  try {
    const u = new URL(raw);
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : null;
  } catch {
    return null;
  }
}

/** File name for display only. The desktop app decides the real name. */
export function displayName(url) {
  try {
    const u = new URL(url);
    const last = decodeURIComponent(u.pathname.split('/').filter(Boolean).pop() || '');
    return last || u.hostname;
  } catch {
    return url;
  }
}

/** Reasons a handoff can fail, as plain sentences. */
export const REASONS = Object.freeze({
  'no-host': 'Quorix desktop was not found. The browser keeps this download.',
  'timeout': 'Quorix desktop did not answer in time. The browser keeps this download.',
  'bad-url': 'Only http and https links can be sent to Quorix.',
  'bad-reply': 'Quorix desktop sent an answer this extension does not understand.',
  'rejected': 'Quorix desktop refused this download.'
});
