// SPDX-License-Identifier: GPL-3.0-or-later
import { DEFAULT_SETTINGS, loadSettings } from './protocol.js';

const form = document.getElementById('form');
const saved = document.getElementById('saved');

function fill(settings) {
  form.capture.checked = settings.capture;
  form.fileTypes.value = settings.fileTypes;
  form.minSizeMb.value = settings.minSizeMb;
  form.excludedDomains.value = settings.excludedDomains;
}

/** Normalises what the user typed before it is stored. */
function read() {
  const clean = text => text.toLowerCase().split(/[\s,]+/).map(s => s.replace(/^\.+/, '')).filter(Boolean);
  const hosts = form.excludedDomains.value.toLowerCase().split(/\s+/)
    .map(s => s.replace(/^[a-z]+:\/\//, '').replace(/\/.*$/, '').replace(/^\*\./, ''))
    .filter(s => /^[a-z0-9.-]+$/.test(s));
  return {
    capture: form.capture.checked,
    fileTypes: [...new Set(clean(form.fileTypes.value))].join(' '),
    minSizeMb: Math.max(0, Math.min(100000, Math.round(Number(form.minSizeMb.value) || 0))),
    excludedDomains: [...new Set(hosts)].join('\n')
  };
}

form.addEventListener('submit', async event => {
  event.preventDefault();
  const settings = read();
  await chrome.storage.sync.set(settings);
  fill(settings);
  saved.textContent = 'Saved';
  setTimeout(() => { saved.textContent = ''; }, 2000);
});

document.getElementById('reset').addEventListener('click', () => { fill(DEFAULT_SETTINGS); saved.textContent = 'Defaults restored, not saved yet'; });

(async () => {
  fill(await loadSettings());
  const status = await chrome.runtime.sendMessage({ type: 'status' });
  const chip = document.getElementById('status');
  chip.dataset.state = status.connected ? 'on' : 'off';
  chip.textContent = status.connected ? 'Connected to Quorix desktop' : 'Quorix desktop not found';
})();
