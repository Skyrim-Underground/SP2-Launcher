// SPDX-License-Identifier: AGPL-3.0-or-later
// One server's page: what its backend says about itself, whether this launcher can use it, and the state of its files.
import { hideAccount, initAccount, showAccount } from './account.js';
import * as backend from './backend.js';
import { el, messageOf, progressBar, setNotice, setText, show } from './dom.js';
import { gateNotice, installAction, installSummary, outcomeText, phaseFraction, phaseText } from './gate.js';
import { hidePlay, initPlay, showPlay } from './play.js';

const PROGRESS_MS = 300;

/** @type {string | null} */
let currentId = null;
/** @type {() => void} */
let onBack = () => {};
/** @type {number | null} */
let progressTimer = null;
let polling = false;

/** @param {() => void} back */
export function initServer(back) {
  onBack = back;
  el('back').addEventListener('click', () => onBack());
  el('remove-server').addEventListener('click', () => {
    void remove();
  });
  for (const id of ['server-website', 'server-invite']) {
    el(id).addEventListener('click', () => {
      void openLink(el(id));
    });
  }
  el('install-run').addEventListener('click', () => {
    void startInstall(false);
  });
  el('install-verify').addEventListener('click', () => {
    void startInstall(true);
  });
  el('install-cancel').addEventListener('click', () => {
    void cancelInstall();
  });
  initAccount();
  initPlay();
}

/** @param {string} id */
export function showServer(id) {
  currentId = id;
  show(el('screen-server'), true);
  reset();
  void load(id);
}

export function hideServer() {
  show(el('screen-server'), false);
  stopProgress();
  currentId = null;
}

function reset() {
  setText(el('server-name'), 'Loading');
  setText(el('server-origin'), '');
  setNotice(el('server-gate'), null);
  setNotice(el('server-error'), null);
  show(el('server-details'), false);
  show(el('remove-server'), false);
  show(el('server-install'), false);
  setNotice(el('install-outcome'), null);
  stopProgress();
  hideAccount();
  hidePlay();
}

/** @param {string} id */
async function load(id) {
  try {
    const page = await backend.serverPage(id);
    if (currentId === id) {
      render(page);
    }
  } catch (error) {
    if (currentId === id) {
      setNotice(el('server-error'), messageOf(error));
    }
  }
}

/** @param {import('./backend.js').ServerPage} page */
function render(page) {
  setText(el('server-name'), page.entry.name);
  setText(
    el('server-origin'),
    page.entry.listed ? page.entry.backendUrl : `${page.entry.backendUrl} (added by URL)`,
  );
  show(el('remove-server'), !page.entry.listed);
  setNotice(el('server-gate'), gateNotice(page.check));
  if (page.check.kind !== 'ok') {
    return;
  }
  const info = page.check.info;
  setText(el('server-name'), info.name);
  setText(el('server-description'), info.description);
  setLink(el('server-website'), info.website);
  setLink(el('server-invite'), info.discordInvite);
  setText(el('server-runtime'), info.gameRuntime);
  setText(el('server-protocol'), info.protocolId);
  show(el('server-details'), true);
  showAccount(page.entry.id);
  void loadInstall(page.entry.id);
  showPlay(page.entry.id);
}

/**
 * Shows a running install for this server, or else what an install would do; an install that ended meanwhile is reported here, whichever server it was for.
 * @param {string} id
 */
async function loadInstall(id) {
  show(el('server-install'), true);
  setText(el('install-summary'), 'Checking files');
  setButtons(null, false, false);
  try {
    const progress = await backend.installProgress();
    if (currentId !== id) {
      return;
    }
    if (progress.state === 'running') {
      if (progress.id === id) {
        watchProgress(id);
        return;
      }
      setText(el('install-summary'), 'Another server is being installed; wait for it to finish.');
      return;
    }
    setNotice(el('install-outcome'), outcomeText(progress, id));
    const state = await backend.installState(id);
    if (currentId === id) {
      renderInstall(state);
    }
  } catch (error) {
    if (currentId === id) {
      setNotice(el('server-error'), messageOf(error));
    }
  }
}

/** @param {import('./backend.js').InstallState} state */
function renderInstall(state) {
  setText(el('install-summary'), installSummary(state));
  const list = el('install-native');
  list.replaceChildren();
  if (state.kind === 'ready') {
    setText(el('install-folder'), `Game folder: ${state.gameFolder}`);
    for (const file of state.native) {
      const item = document.createElement('li');
      const path = document.createElement('span');
      path.textContent = file.to;
      const origin = document.createElement('span');
      origin.className = 'muted';
      origin.textContent = ` from ${file.origin}`;
      item.append(path, origin);
      list.append(item);
    }
  } else {
    setText(el('install-folder'), '');
  }
  show(el('install-native-title'), state.kind === 'ready' && state.native.length > 0);
  setButtons(installAction(state), state.kind === 'ready' && state.installedVersion !== null, false);
  show(el('install-progress'), false);
}

/**
 * @param {string | null} action the label of the main button, or null to hide it
 * @param {boolean} verify
 * @param {boolean} cancel
 */
function setButtons(action, verify, cancel) {
  const run = el('install-run');
  setText(run, action ?? '');
  show(run, action !== null);
  show(el('install-verify'), verify);
  show(el('install-cancel'), cancel);
}

/** @param {boolean} verify */
async function startInstall(verify) {
  if (currentId === null) {
    return;
  }
  const id = currentId;
  setNotice(el('install-outcome'), null);
  try {
    await backend.installStart(id, verify);
    if (currentId === id) {
      watchProgress(id);
    }
  } catch (error) {
    if (currentId === id) {
      setNotice(el('server-error'), messageOf(error));
    }
  }
}

async function cancelInstall() {
  try {
    await backend.installCancel();
  } catch (error) {
    setNotice(el('server-error'), messageOf(error));
  }
}

/** @param {string} id */
function watchProgress(id) {
  setButtons(null, false, true);
  show(el('install-progress'), true);
  stopProgress();
  progressTimer = window.setInterval(() => {
    void pollProgress(id);
  }, PROGRESS_MS);
  void pollProgress(id);
}

function stopProgress() {
  if (progressTimer !== null) {
    window.clearInterval(progressTimer);
    progressTimer = null;
  }
}

/** @param {string} id */
async function pollProgress(id) {
  if (polling) {
    return;
  }
  polling = true;
  try {
    const progress = await backend.installProgress();
    if (currentId !== id) {
      stopProgress();
      return;
    }
    if (progress.state === 'running') {
      renderPhase(progress.phase);
      return;
    }
    stopProgress();
    show(el('install-progress'), false);
    setNotice(el('install-outcome'), outcomeText(progress, id));
    const state = await backend.installState(id);
    if (currentId === id) {
      renderInstall(state);
    }
  } catch (error) {
    stopProgress();
    if (currentId === id) {
      setNotice(el('server-error'), messageOf(error));
    }
  } finally {
    polling = false;
  }
}

/** @param {import('./backend.js').Phase} phase */
function renderPhase(phase) {
  setText(el('install-phase'), phaseText(phase));
  const bar = progressBar('install-bar');
  const fraction = phaseFraction(phase);
  if (fraction === null) {
    bar.removeAttribute('value');
  } else {
    bar.value = fraction;
  }
}

/**
 * @param {HTMLElement} button
 * @param {string} url
 */
function setLink(button, url) {
  button.textContent = url;
  button.dataset.url = url;
  show(button, url !== '');
}

/** @param {HTMLElement} button */
async function openLink(button) {
  const url = button.dataset.url;
  if (!url) {
    return;
  }
  try {
    await backend.openLink(url);
  } catch (error) {
    setNotice(el('server-error'), messageOf(error));
  }
}

async function remove() {
  if (currentId === null || !window.confirm('Remove this server from the list?')) {
    return;
  }
  try {
    await backend.removeServer(currentId);
    onBack();
  } catch (error) {
    setNotice(el('server-error'), messageOf(error));
  }
}
