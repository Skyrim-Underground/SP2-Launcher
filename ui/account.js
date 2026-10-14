// SPDX-License-Identifier: AGPL-3.0-or-later
// The account panel of a server's page: the stored sign-in, Sign in and Sign out, and a sign-in waiting for the browser.
import * as backend from './backend.js';
import { el, messageOf, setNotice, setText, show } from './dom.js';
import { accountText, signInProgressText } from './gate.js';

const PROGRESS_MS = 500;

/** @type {string | null} */
let currentId = null;
/** @type {number | null} */
let timer = null;
let polling = false;

export function initAccount() {
  el('signin').addEventListener('click', () => {
    void start();
  });
  el('signout').addEventListener('click', () => {
    void signOut();
  });
  el('signin-cancel').addEventListener('click', () => {
    void cancel();
  });
}

/** @param {string} id */
export function showAccount(id) {
  currentId = id;
  show(el('server-account'), true);
  setText(el('account-summary'), 'Checking the sign-in');
  setNotice(el('account-notice'), null);
  setButtons(false, false, false);
  void load(id);
}

export function hideAccount() {
  show(el('server-account'), false);
  stopTimer();
  currentId = null;
}

/** Asks the server again; a Play that found the sign-in gone or banned shows here afterwards. */
export function refreshAccount() {
  if (currentId !== null && timer === null) {
    void load(currentId);
  }
}

/**
 * Shows a sign-in waiting for the browser, or else what the server says about the stored one; an ending is shown here.
 * @param {string} id
 */
async function load(id) {
  try {
    const progress = await backend.signInProgress();
    if (currentId !== id) {
      return;
    }
    if (progress.state === 'waiting') {
      renderWaiting(progress, id);
      return;
    }
    setNotice(el('account-notice'), signInProgressText(progress, id));
    const view = await backend.signInState(id);
    if (currentId === id) {
      render(view);
    }
  } catch (error) {
    if (currentId === id) {
      setNotice(el('account-notice'), messageOf(error));
    }
  }
}

/** @param {import('./backend.js').SignInView} view */
function render(view) {
  setText(el('account-summary'), accountText(view));
  const stored = view.kind === 'signedIn' || view.kind === 'unchecked';
  setButtons(!stored, stored, false);
}

/**
 * @param {import('./backend.js').SignInProgress} progress
 * @param {string} id
 */
function renderWaiting(progress, id) {
  setText(el('account-summary'), signInProgressText(progress, id) ?? '');
  setButtons(false, false, progress.state === 'waiting' && progress.id === id);
  stopTimer();
  timer = window.setInterval(() => {
    void poll(id);
  }, PROGRESS_MS);
}

function stopTimer() {
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

/**
 * @param {boolean} signIn
 * @param {boolean} signOut
 * @param {boolean} cancel
 */
function setButtons(signIn, signOut, cancel) {
  show(el('signin'), signIn);
  show(el('signout'), signOut);
  show(el('signin-cancel'), cancel);
}

/** @param {string} id */
async function poll(id) {
  if (polling) {
    return;
  }
  polling = true;
  try {
    const progress = await backend.signInProgress();
    if (currentId !== id) {
      stopTimer();
      return;
    }
    if (progress.state === 'waiting') {
      return;
    }
    stopTimer();
    setNotice(el('account-notice'), signInProgressText(progress, id));
    const view = await backend.signInState(id);
    if (currentId === id) {
      render(view);
    }
  } catch (error) {
    stopTimer();
    if (currentId === id) {
      setNotice(el('account-notice'), messageOf(error));
    }
  } finally {
    polling = false;
  }
}

async function start() {
  if (currentId === null) {
    return;
  }
  const id = currentId;
  setNotice(el('account-notice'), null);
  try {
    await backend.signInStart(id);
    if (currentId === id) {
      renderWaiting({ state: 'waiting', id }, id);
    }
  } catch (error) {
    if (currentId === id) {
      setNotice(el('account-notice'), messageOf(error));
    }
  }
}

async function cancel() {
  if (currentId === null) {
    return;
  }
  const id = currentId;
  try {
    await backend.signInCancel();
  } catch (error) {
    setNotice(el('account-notice'), messageOf(error));
  }
  stopTimer();
  if (currentId === id) {
    void load(id);
  }
}

async function signOut() {
  if (currentId === null) {
    return;
  }
  const id = currentId;
  try {
    await backend.signOut(id);
  } catch (error) {
    setNotice(el('account-notice'), messageOf(error));
  }
  if (currentId === id) {
    void load(id);
  }
}
