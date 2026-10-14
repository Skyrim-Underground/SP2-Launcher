// SPDX-License-Identifier: AGPL-3.0-or-later
// Entry point: the update check comes first, then one screen is visible at a time and the top bar switches between them.
import { initAdd } from './add.js';
import * as backend from './backend.js';
import { button, el, messageOf, setNotice, setText } from './dom.js';
import { updateNotice } from './gate.js';
import { hideList, initList, showList } from './list.js';
import { hideServer, initServer, showServer } from './server.js';
import { hideSettings, initSettings, showSettings } from './settings.js';
import { hideUpdate, initUpdate, showUpdate } from './update.js';

/** @typedef {{ kind: 'list', refresh: boolean } | { kind: 'server', id: string } | { kind: 'settings' } | { kind: 'update', version: string }} Screen */

/** @type {import('./backend.js').LauncherInfo | null} */
let info = null;

/** @param {Screen} screen */
function switchTo(screen) {
  hideList();
  hideServer();
  hideSettings();
  hideUpdate();
  const updating = screen.kind === 'update';
  for (const id of ['nav-servers', 'nav-settings']) {
    button(id).disabled = updating;
  }
  el('nav-servers').classList.toggle('active', screen.kind !== 'settings' && !updating);
  el('nav-settings').classList.toggle('active', screen.kind === 'settings');
  switch (screen.kind) {
    case 'list':
      showList(screen.refresh);
      break;
    case 'server':
      showServer(screen.id);
      break;
    case 'settings':
      showSettings();
      break;
    case 'update':
      showUpdate(screen.version);
      break;
  }
}

/** @param {'website' | 'rulesUrl'} key */
async function openInfoLink(key) {
  if (info === null) {
    return;
  }
  try {
    await backend.openLink(info[key]);
  } catch (error) {
    setNotice(el('list-error'), messageOf(error));
  }
}

/**
 * The first screen: the update when a newer build is announced, otherwise the list, with a check that failed noted on it.
 * @returns {Promise<Screen>}
 */
async function firstScreen() {
  try {
    const check = await backend.updateCheck();
    if (check.kind === 'available') {
      return { kind: 'update', version: check.version };
    }
    setNotice(el('update-notice'), updateNotice(check));
  } catch (error) {
    setNotice(el('update-notice'), `Updates could not be checked: ${messageOf(error)}`);
  }
  return { kind: 'list', refresh: true };
}

initList((id) => switchTo({ kind: 'server', id }));
initServer(() => switchTo({ kind: 'list', refresh: false }));
initAdd(() => switchTo({ kind: 'list', refresh: false }));
initSettings();
initUpdate(() => switchTo({ kind: 'list', refresh: true }));
el('nav-servers').addEventListener('click', () => switchTo({ kind: 'list', refresh: false }));
el('nav-settings').addEventListener('click', () => switchTo({ kind: 'settings' }));
el('link-website').addEventListener('click', () => {
  void openInfoLink('website');
});
el('link-rules').addEventListener('click', () => {
  void openInfoLink('rulesUrl');
});

try {
  info = await backend.launcherInfo();
  setText(el('launcher-version'), `Version ${info.version}`);
} catch (error) {
  setNotice(el('list-error'), messageOf(error));
}
switchTo(await firstScreen());
