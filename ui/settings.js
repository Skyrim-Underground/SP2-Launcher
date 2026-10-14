// SPDX-License-Identifier: AGPL-3.0-or-later
// The settings screen: the base folder under which every server gets its own game folder.
import * as backend from './backend.js';
import { el, input, messageOf, setNotice, setText, show } from './dom.js';

export function initSettings() {
  el('save-base-folder').addEventListener('click', () => {
    void save();
  });
}

export function showSettings() {
  show(el('screen-settings'), true);
  void load();
}

export function hideSettings() {
  show(el('screen-settings'), false);
}

/** @param {import('./backend.js').Settings} settings */
function render(settings) {
  input('base-folder').value = settings.baseFolder ?? '';
  setText(
    el('settings-default'),
    settings.defaultBaseFolder === null
      ? 'No Steam copy of the game was found, so a folder must be set here.'
      : `Leave it empty to use ${settings.defaultBaseFolder}.`,
  );
}

async function load() {
  setNotice(el('settings-message'), null);
  try {
    render(await backend.getSettings());
  } catch (error) {
    setNotice(el('settings-message'), messageOf(error));
  }
}

async function save() {
  try {
    render(await backend.setBaseFolder(input('base-folder').value));
    setNotice(el('settings-message'), 'Saved.');
  } catch (error) {
    setNotice(el('settings-message'), messageOf(error));
  }
}
