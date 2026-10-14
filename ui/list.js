// SPDX-License-Identifier: AGPL-3.0-or-later
// The server list: entries from the directory and by URL, with each backend's status polled while the list is open.
import * as backend from './backend.js';
import { el, messageOf, setNotice, show } from './dom.js';
import { gateNotice, listNotice, statusText } from './gate.js';

const POLL_MS = 30_000;

/** @type {number | null} */
let timer = null;
/** @type {(id: string) => void} */
let onSelect = () => {};

/** @param {(id: string) => void} select */
export function initList(select) {
  onSelect = select;
  el('refresh').addEventListener('click', () => {
    void load(true);
  });
}

/**
 * A refresh fetches the directory and every server's API version; otherwise the last refresh is shown again.
 * @param {boolean} refresh
 */
export function showList(refresh) {
  show(el('screen-list'), true);
  void load(refresh);
  timer = window.setInterval(() => {
    void poll();
  }, POLL_MS);
}

export function hideList() {
  show(el('screen-list'), false);
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

/** @param {boolean} refresh */
async function load(refresh) {
  setNotice(el('list-error'), null);
  try {
    const list = await (refresh ? backend.loadServers() : backend.listServers());
    render(list);
    await poll();
  } catch (error) {
    setNotice(el('list-error'), messageOf(error));
  }
}

/** @param {import('./backend.js').ServerList} list */
function render(list) {
  const container = el('server-list');
  container.replaceChildren();
  for (const row of list.servers) {
    const item = document.createElement('li');
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'server';
    button.dataset.id = row.entry.id;
    const name = document.createElement('span');
    name.className = 'name';
    name.textContent = row.entry.name;
    const origin = document.createElement('span');
    origin.className = 'origin';
    origin.textContent = row.entry.listed
      ? row.entry.backendUrl
      : `${row.entry.backendUrl} (added by URL)`;
    const status = document.createElement('span');
    status.className = 'status';
    status.textContent = 'Checking';
    const gate = document.createElement('span');
    gate.className = 'gate';
    setNotice(gate, row.check === null ? null : gateNotice(row.check));
    button.append(name, origin, status, gate);
    button.addEventListener('click', () => onSelect(row.entry.id));
    item.append(button);
    container.append(item);
  }
  show(el('list-empty'), list.servers.length === 0);
  setNotice(el('list-notice'), listNotice(list));
}

async function poll() {
  try {
    const rows = await backend.serverStatus();
    for (const row of rows) {
      const node = el('server-list').querySelector(`[data-id="${CSS.escape(row.id)}"] .status`);
      if (node) {
        node.textContent = statusText(row.status);
      }
    }
  } catch (error) {
    setNotice(el('list-error'), messageOf(error));
  }
}
