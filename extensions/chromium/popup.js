// SPDX-License-Identifier: GPL-3.0-or-later
import { loadSettings, validUrl, REASONS } from './protocol.js';

const $ = id => document.getElementById(id);
const ask = message => chrome.runtime.sendMessage(message);

function setStatus(status) {
  const chip = $('status');
  chip.dataset.state = status.connected ? 'on' : 'off';
  chip.textContent = status.connected ? 'Connected' : 'Not connected';
  $('missing').hidden = status.connected;
}

async function showRecent() {
  const recent = await ask({ type: 'recent' });
  $('recent-wrap').hidden = recent.length === 0;
  const list = $('recent');
  list.replaceChildren(...recent.slice(0, 5).map(item => {
    const li = document.createElement('li');
    li.dataset.ok = item.ok;
    const name = document.createElement('b');
    name.textContent = item.name;
    const result = document.createElement('small');
    result.textContent = item.message;
    li.append(name, result);
    return li;
  }));
}

$('send').addEventListener('submit', async event => {
  event.preventDefault();
  const note = $('note');
  const url = validUrl($('url').value.trim());
  if (!url) { note.dataset.ok = 'false'; note.textContent = REASONS['bad-url']; return; }

  note.dataset.ok = ''; note.textContent = 'Sending';
  const reply = await ask({ type: 'handoff', url });
  note.dataset.ok = String(reply.ok);
  note.textContent = reply.ok ? 'Sent to Quorix' : reply.message;
  if (reply.ok) $('url').value = '';
  showRecent();
});

$('capture').addEventListener('change', event => chrome.storage.sync.set({ capture: event.target.checked }));
$('options').addEventListener('click', () => chrome.runtime.openOptionsPage());

(async () => {
  $('capture').checked = (await loadSettings()).capture;
  showRecent();
  setStatus(await ask({ type: 'status' }));
})();
