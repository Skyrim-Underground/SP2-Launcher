// SPDX-License-Identifier: AGPL-3.0-or-later
// The update screen: shown before anything else when a newer build is announced, until the installer takes over or the update fails.
import * as backend from './backend.js';
import { el, messageOf, progressBar, setText, show } from './dom.js';
import { updateFraction, updateText } from './gate.js';

const POLL_MS = 300;

/** @type {number | null} */
let timer = null;
/** @type {() => void} */
let onContinue = () => {};

/** @param {() => void} cont */
export function initUpdate(cont) {
  onContinue = cont;
  el('update-continue').addEventListener('click', () => onContinue());
}

/**
 * Starts the install of the pending update and follows it until the installer runs or the update fails.
 * @param {string} version
 */
export function showUpdate(version) {
  show(el('screen-update'), true);
  render({ state: 'idle' }, version);
  void start(version);
}

export function hideUpdate() {
  show(el('screen-update'), false);
  stopPolling();
}

function stopPolling() {
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

/**
 * @param {import('./backend.js').UpdateProgress} progress
 * @param {string} version
 */
function render(progress, version) {
  const failed = progress.state === 'failed';
  const text = el('update-text');
  setText(text, updateText(progress, version));
  text.classList.toggle('error', failed);
  const bar = progressBar('update-bar');
  const fraction = updateFraction(progress);
  if (fraction === null) {
    bar.removeAttribute('value');
  } else {
    bar.value = fraction;
  }
  show(bar, !failed);
  show(el('update-continue'), failed);
}

/** @param {string} version */
async function start(version) {
  try {
    await backend.updateInstall();
  } catch (error) {
    render({ state: 'failed', message: messageOf(error) }, version);
    return;
  }
  timer = window.setInterval(() => {
    void poll(version);
  }, POLL_MS);
}

/** @param {string} version */
async function poll(version) {
  try {
    const progress = await backend.updateProgress();
    render(progress, version);
    if (progress.state === 'failed') {
      stopPolling();
    }
  } catch (error) {
    stopPolling();
    render({ state: 'failed', message: messageOf(error) }, version);
  }
}
