// SPDX-License-Identifier: AGPL-3.0-or-later
// The Play panel of a server's page: Play, the steps of a start, Reconnect while the game runs, and how the last run ended.
import { refreshAccount } from './account.js';
import * as backend from './backend.js';
import { el, messageOf, setNotice, show } from './dom.js';
import { playButtons, playNoticeText, playPhaseText } from './gate.js';

const POLL_MS = 1000;

/** @type {string | null} */
let currentId = null;
/** @type {number | null} */
let timer = null;
let busy = false;

export function initPlay() {
  el('play-run').addEventListener('click', () => {
    void play();
  });
  el('play-reconnect').addEventListener('click', () => {
    void reconnect();
  });
}

/** @param {string} id */
export function showPlay(id) {
  currentId = id;
  show(el('server-play'), true);
  setNotice(el('play-phase'), null);
  setNotice(el('play-notice'), null);
  setButtons(false, false);
  void refresh(id);
  timer = window.setInterval(() => {
    void refresh(id);
  }, POLL_MS);
}

export function hidePlay() {
  show(el('server-play'), false);
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
  currentId = null;
}

/**
 * @param {boolean} play
 * @param {boolean} reconnect
 */
function setButtons(play, reconnect) {
  show(el('play-run'), play);
  show(el('play-reconnect'), reconnect);
}

/**
 * @param {import('./backend.js').PlayView} view
 * @param {string} id
 */
function render(view, id) {
  setNotice(el('play-phase'), playPhaseText(view, id));
  setNotice(el('play-notice'), view.notice === null ? null : playNoticeText(view.notice));
  const buttons = playButtons(view, id);
  setButtons(buttons.play && !busy, buttons.reconnect && !busy);
}

/** @param {string} id */
async function refresh(id) {
  if (busy) {
    return;
  }
  try {
    const view = await backend.playState();
    if (currentId === id) {
      render(view, id);
    }
  } catch (error) {
    if (currentId === id) {
      setNotice(el('play-notice'), messageOf(error));
    }
  }
}

/**
 * Runs one command that moves the game run and shows where it left things.
 * @param {(id: string) => Promise<import('./backend.js').PlayView>} command
 */
async function drive(command) {
  if (currentId === null || busy) {
    return;
  }
  const id = currentId;
  busy = true;
  setButtons(false, false);
  setNotice(el('play-notice'), null);
  /** @type {import('./backend.js').PlayView | null} */
  let view = null;
  try {
    view = await command(id);
  } catch (error) {
    if (currentId === id) {
      setNotice(el('play-notice'), messageOf(error));
    }
  }
  busy = false;
  if (currentId !== id) {
    return;
  }
  if (view === null) {
    void refresh(id);
  } else {
    render(view, id);
  }
  refreshAccount();
}

function play() {
  return drive((id) => backend.play(id));
}

function reconnect() {
  return drive((id) => backend.reconnect(id));
}
