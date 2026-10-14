// SPDX-License-Identifier: AGPL-3.0-or-later
// The add-by-URL dialog: the launcher checks the origin, then the server's own details and the warning are shown before the confirm.
import * as backend from './backend.js';
import { dialog, el, input, messageOf, setNotice, setText, show } from './dom.js';

/** @type {() => void} */
let onAdded = () => {};

/** @param {() => void} added */
export function initAdd(added) {
  onAdded = added;
  el('add-server').addEventListener('click', open);
  el('add-cancel').addEventListener('click', () => dialog('add-dialog').close());
  el('add-confirm').addEventListener('click', () => {
    void confirm();
  });
  el('add-form').addEventListener('submit', (event) => {
    event.preventDefault();
    void check();
  });
  input('add-url').addEventListener('input', () => {
    show(el('add-preview'), false);
    show(el('add-confirm'), false);
  });
}

function open() {
  input('add-url').value = '';
  setNotice(el('add-error'), null);
  show(el('add-preview'), false);
  show(el('add-confirm'), false);
  dialog('add-dialog').showModal();
  input('add-url').focus();
}

async function check() {
  setNotice(el('add-error'), null);
  try {
    const preview = await backend.previewServer(input('add-url').value);
    setText(el('add-name'), preview.name);
    setText(el('add-description'), preview.description);
    setText(el('add-origin'), preview.origin);
    setText(el('add-website'), preview.website);
    setText(el('add-warning'), preview.warning);
    show(el('add-preview'), true);
    show(el('add-confirm'), true);
  } catch (error) {
    setNotice(el('add-error'), messageOf(error));
  }
}

async function confirm() {
  try {
    await backend.addServer(input('add-url').value);
    dialog('add-dialog').close();
    onAdded();
  } catch (error) {
    setNotice(el('add-error'), messageOf(error));
  }
}
