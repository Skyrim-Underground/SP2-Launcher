// SPDX-License-Identifier: AGPL-3.0-or-later
// Presentation rules for what the backend reports, kept free of the DOM so they can be tested alone.

/** @typedef {import('./backend.js').ServerCheck} ServerCheck */
/** @typedef {import('./backend.js').ServerList} ServerList */
/** @typedef {import('./backend.js').Status} Status */
/** @typedef {import('./backend.js').DirectoryReport} DirectoryReport */
/** @typedef {import('./backend.js').InstallState} InstallState */
/** @typedef {import('./backend.js').Phase} Phase */
/** @typedef {import('./backend.js').InstallProgress} InstallProgress */
/** @typedef {import('./backend.js').Access} Access */
/** @typedef {import('./backend.js').SignInView} SignInView */
/** @typedef {import('./backend.js').SignInProgress} SignInProgress */
/** @typedef {import('./backend.js').PlayView} PlayView */
/** @typedef {import('./backend.js').PlayNotice} PlayNotice */
/** @typedef {import('./backend.js').UpdateCheck} UpdateCheck */
/** @typedef {import('./backend.js').UpdateProgress} UpdateProgress */

const UNITS = ['bytes', 'KiB', 'MiB', 'GiB'];

/**
 * The notice shown instead of the server details, or null when this launcher can use the server.
 * @param {ServerCheck} check
 * @returns {string | null}
 */
export function gateNotice(check) {
  switch (check.kind) {
    case 'ok':
      return null;
    case 'launcherTooOld':
      return `Needs a newer launcher: this server speaks API version ${check.apiVersion}.`;
    case 'serverTooOld':
      return `Server too old: it speaks API version ${check.apiVersion}, which this launcher no longer supports.`;
  }
}

/**
 * @param {Status | null} status
 * @returns {string}
 */
export function statusText(status) {
  if (status === null) {
    return 'Unreachable';
  }
  if (!status.online) {
    return 'Offline';
  }
  return `Online, ${status.players} of ${status.maxPlayers} players`;
}

/**
 * Where the entries came from, or null when the directory is fresh or has not been fetched yet.
 * @param {DirectoryReport | null} report
 * @returns {string | null}
 */
export function directoryNotice(report) {
  if (report === null) {
    return null;
  }
  switch (report.state) {
    case 'fresh':
      return null;
    case 'cached':
      return `The directory could not be refreshed (${report.reason}), so the last good copy is shown.`;
    case 'unavailable':
      return `The directory is unavailable (${report.reason}). Servers added by URL still work.`;
    case 'unconfigured':
      return 'This build has no directory signing key, so only servers added by URL are shown.';
  }
}

/**
 * The lines above the list: files set aside at the start, then the directory state; null when there is nothing to say.
 * @param {ServerList} list
 * @returns {string | null}
 */
export function listNotice(list) {
  const lines = [...list.setAside];
  const directory = directoryNotice(list.directory);
  if (directory !== null) {
    lines.push(directory);
  }
  return lines.length === 0 ? null : lines.join('\n');
}

/**
 * @param {number} bytes
 * @returns {string}
 */
export function formatBytes(bytes) {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${value} bytes` : `${value.toFixed(1)} ${UNITS[unit]}`;
}

/**
 * @param {number} count
 * @param {string} noun
 * @returns {string}
 */
function counted(count, noun) {
  return `${count} ${noun}${count === 1 ? '' : 's'}`;
}

/**
 * One sentence on what the server's files are to this PC: why they cannot be installed, or what Install would do.
 * @param {InstallState} state
 * @returns {string}
 */
export function installSummary(state) {
  switch (state.kind) {
    case 'unavailable':
      return `Cannot install: ${state.reason}`;
    case 'refused':
      return `The server's file list was refused: ${state.reason}`;
    case 'ready': {
      const removal = state.toRemove > 0 ? `, ${counted(state.toRemove, 'file')} to remove` : '';
      const placing = state.toPlace > 0 ? `, ${counted(state.toPlace, 'game file')} to place` : '';
      const work = `${counted(state.toDownload, 'file')} to download (${formatBytes(state.bytes)})${removal}${placing}`;
      if (state.installedVersion === null) {
        return `Not installed. Version ${state.version}: ${work}.`;
      }
      if (!hasWork(state)) {
        return `Installed, version ${state.installedVersion}. Up to date.`;
      }
      return `Installed, version ${state.installedVersion}. Update to ${state.version}: ${work}.`;
    }
  }
}

/** @param {Extract<InstallState, { kind: 'ready' }>} state */
function hasWork(state) {
  return state.toDownload > 0 || state.toRemove > 0 || state.toPlace > 0;
}

/**
 * The label of the main button, or null when there is nothing to run.
 * @param {InstallState} state
 * @returns {'Install' | 'Update' | null}
 */
export function installAction(state) {
  if (state.kind !== 'ready') {
    return null;
  }
  if (state.installedVersion === null) {
    return 'Install';
  }
  return hasWork(state) ? 'Update' : null;
}

/**
 * @param {Access} access
 * @returns {string}
 */
function accessText(access) {
  switch (access) {
    case 'ok':
      return 'You may play.';
    case 'not_member':
      return "You are not a member of this server's Discord, so you cannot play yet.";
    case 'missing_role':
      return 'Your Discord account has no role that allows playing on this server.';
    case 'banned':
      return 'You are banned from this server.';
    case 'discord_unavailable':
      return 'The server cannot reach Discord right now, so it cannot say whether you may play.';
  }
}

/**
 * One line on the account for the server shown.
 * @param {SignInView} view
 * @returns {string}
 */
export function accountText(view) {
  switch (view.kind) {
    case 'signedOut':
      return 'Not signed in. Sign in with Discord to play on this server.';
    case 'signedIn':
      return `Signed in as ${view.displayName}. ${accessText(view.access)}`;
    case 'banned': {
      const until = view.banUntil === null ? '' : ` until ${new Date(view.banUntil).toLocaleString()}`;
      const reason = view.banReason === null ? '' : ` Reason: ${view.banReason}`;
      return `${view.displayName} is banned from this server${until}.${reason}`;
    }
    case 'unchecked':
      return `Signed in as ${view.displayName}, but the server could not be asked whether you may play: ${view.reason}`;
  }
}

/**
 * What the account panel says while a sign-in waits for the browser or once one ended; null when none is going on.
 * @param {SignInProgress} progress
 * @param {string} shownId
 * @returns {string | null}
 */
export function signInProgressText(progress, shownId) {
  if (progress.state === 'idle') {
    return null;
  }
  const own = progress.id === shownId;
  const prefix = own ? '' : `${progress.id}: `;
  switch (progress.state) {
    case 'waiting':
      return own
        ? 'Waiting for the browser. Finish the Discord sign-in there, then come back here.'
        : `A sign-in for ${progress.id} is waiting for the browser; finish or cancel it first.`;
    case 'done':
      return `${prefix}Signed in as ${progress.displayName}.`;
    case 'failed':
      return `${prefix}The sign-in failed: ${progress.message}`;
  }
}

/**
 * What the game run is doing, or null when nothing runs.
 * @param {PlayView} view
 * @param {string} shownId
 * @returns {string | null}
 */
export function playPhaseText(view, shownId) {
  const elsewhere = view.id !== null && view.id !== shownId ? ` for ${view.id}` : '';
  switch (view.state) {
    case 'idle':
      return null;
    case 'verifyingFiles':
      return `Checking the server's files${elsewhere}.`;
    case 'requestingToken':
      return `Asking the server for a play token${elsewhere}.`;
    case 'launching':
      return `Starting the game${elsewhere}.`;
    case 'running':
      return `The game is running${elsewhere}. Keep the launcher open; press Reconnect if the game lost its connection.`;
    case 'reconnecting':
      return `Asking the server for a new play token${elsewhere}.`;
  }
}

/**
 * How the last step of a game run ended.
 * @param {PlayNotice} notice
 * @returns {string}
 */
export function playNoticeText(notice) {
  switch (notice.kind) {
    case 'failed':
      return `Play failed: ${notice.message}`;
    case 'reconnected':
      return 'A new play token is in place; the game reconnects with it.';
    case 'exited': {
      const exit = notice.code === null || notice.code === 0 ? 'The game exited.' : `The game exited with code ${notice.code}.`;
      return notice.problem === null
        ? exit
        : `${exit} Its files could not be put back (${notice.problem}); the next launcher start puts them back.`;
    }
  }
}

/**
 * Play while nothing runs anywhere, Reconnect while this server's game runs.
 * @param {PlayView} view
 * @param {string} shownId
 * @returns {{ play: boolean, reconnect: boolean }}
 */
export function playButtons(view, shownId) {
  return {
    play: view.state === 'idle',
    reconnect: view.state === 'running' && view.id === shownId,
  };
}

/**
 * @param {Phase} phase
 * @returns {string}
 */
export function phaseText(phase) {
  switch (phase.phase) {
    case 'planning':
      return 'Checking files';
    case 'removing':
      return `Removing files (${phase.done} of ${phase.total})`;
    case 'linking':
      return `Linking game files (${phase.done} of ${phase.total})`;
    case 'downloading':
      return `Downloading ${phase.file} (file ${phase.done + 1} of ${phase.total}, ${formatBytes(phase.bytesDone)} of ${formatBytes(phase.bytesTotal)})`;
    case 'finishing':
      return 'Finishing';
  }
}

/**
 * The share of the phase that is done, or null when the phase has no measure.
 * @param {Phase} phase
 * @returns {number | null}
 */
export function phaseFraction(phase) {
  switch (phase.phase) {
    case 'removing':
    case 'linking':
      return phase.total === 0 ? 1 : phase.done / phase.total;
    case 'downloading':
      return phase.bytesTotal === 0 ? 1 : phase.bytesDone / phase.bytesTotal;
    default:
      return null;
  }
}

/**
 * How the last install ended, or null while one runs or none has run; named after its server when that is not the one shown.
 * @param {InstallProgress} progress
 * @param {string} shownId
 * @returns {string | null}
 */
export function outcomeText(progress, shownId) {
  if (progress.state === 'idle' || progress.state === 'running') {
    return null;
  }
  const prefix = progress.id === shownId ? '' : `${progress.id}: `;
  switch (progress.state) {
    case 'done': {
      const report = progress.report;
      const line = `${prefix}Files are in place: ${report.downloaded} downloaded, ${report.kept} kept, ${report.removed} removed, ${report.linked} linked, ${report.copied} copied.`;
      return [line, ...report.notices].join('\n');
    }
    case 'failed':
      return `${prefix}The install failed: ${progress.message}`;
    case 'cancelled':
      return `${prefix}The install was cancelled. Files already in place stay, and the next install carries on from them.`;
  }
}

/**
 * Why this build goes on without an update, or null when there is nothing to say.
 * @param {UpdateCheck} check
 * @returns {string | null}
 */
export function updateNotice(check) {
  return check.kind === 'skipped' ? `Updates could not be checked: ${check.reason}` : null;
}

/**
 * One line on the update to `version`: what is being done, or why it stopped.
 * @param {UpdateProgress} progress
 * @param {string} version
 * @returns {string}
 */
export function updateText(progress, version) {
  switch (progress.state) {
    case 'idle':
      return `Version ${version} is available and is installed before anything else.`;
    case 'downloading': {
      const done = formatBytes(progress.bytesDone);
      return progress.bytesTotal === null
        ? `Downloading version ${version} (${done})`
        : `Downloading version ${version} (${done} of ${formatBytes(progress.bytesTotal)})`;
    }
    case 'installing':
      return `Starting the installer for version ${version}. The launcher closes, and the new version opens when the installer is done.`;
    case 'failed':
      return `The update to version ${version} failed: ${progress.message}`;
  }
}

/**
 * The share of the download that is done, 1 once the installer starts, or null while there is no measure.
 * @param {UpdateProgress} progress
 * @returns {number | null}
 */
export function updateFraction(progress) {
  switch (progress.state) {
    case 'downloading':
      return progress.bytesTotal === null || progress.bytesTotal === 0 ? null : progress.bytesDone / progress.bytesTotal;
    case 'installing':
      return 1;
    default:
      return null;
  }
}
