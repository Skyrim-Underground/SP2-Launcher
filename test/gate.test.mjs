// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  accountText,
  directoryNotice,
  formatBytes,
  gateNotice,
  installAction,
  installSummary,
  listNotice,
  outcomeText,
  phaseFraction,
  phaseText,
  playButtons,
  playNoticeText,
  playPhaseText,
  signInProgressText,
  statusText,
  updateFraction,
  updateNotice,
  updateText,
} from '../ui/gate.js';

const info = {
  name: 'Known',
  description: 'd',
  website: 'https://www.example.org',
  discordInvite: '',
  protocolId: 'sug1_',
  gameRuntime: 'skyrimse-1.7.104',
  apiVersion: 1,
  manifestUrl: '/api/v1/manifest',
};

/** @type {import('../ui/backend.js').InstallState} */
const ready = {
  kind: 'ready',
  version: '2',
  installedVersion: null,
  native: [{ to: 'skse64_loader.exe', origin: 'https://files.example.org' }],
  toDownload: 3,
  bytes: 1536,
  toRemove: 0,
  toPlace: 0,
  gameFolder: 'C:\\Games\\servers\\x',
};

test('a known API version shows the server details', () => {
  assert.equal(gateNotice({ kind: 'ok', info }), null);
});

test('a newer API version needs a newer launcher', () => {
  const notice = gateNotice({ kind: 'launcherTooOld', apiVersion: 2 });
  assert.match(notice ?? '', /needs a newer launcher/i);
  assert.match(notice ?? '', /version 2/);
});

test('an older API version marks the server as too old', () => {
  const notice = gateNotice({ kind: 'serverTooOld', apiVersion: 0 });
  assert.match(notice ?? '', /server too old/i);
});

test('status text covers unreachable, offline and online', () => {
  assert.equal(statusText(null), 'Unreachable');
  assert.equal(statusText({ online: false, players: 0, maxPlayers: 64, signIn: 'ok' }), 'Offline');
  assert.equal(
    statusText({ online: true, players: 3, maxPlayers: 64, signIn: 'ok' }),
    'Online, 3 of 64 players',
  );
});

test('the directory notice names the state', () => {
  assert.equal(directoryNotice(null), null);
  assert.equal(directoryNotice({ state: 'fresh' }), null);
  assert.match(directoryNotice({ state: 'cached', reason: 'x' }) ?? '', /last good copy/);
  assert.match(directoryNotice({ state: 'unavailable', reason: 'x' }) ?? '', /unavailable/);
  assert.match(directoryNotice({ state: 'unconfigured' }) ?? '', /signing key/);
});

test('the list notice puts set-aside files before the directory state', () => {
  assert.equal(listNotice({ servers: [], directory: { state: 'fresh' }, setAside: [] }), null);
  assert.equal(listNotice({ servers: [], directory: null, setAside: [] }), null);
  assert.equal(
    listNotice({ servers: [], directory: { state: 'fresh' }, setAside: ['servers.json was renamed'] }),
    'servers.json was renamed',
  );
  const both = listNotice({
    servers: [],
    directory: { state: 'unconfigured' },
    setAside: ['one', 'two'],
  });
  assert.equal(both?.split('\n').length, 3);
  assert.match(both ?? '', /^one\ntwo\n.*signing key/);
});

test('bytes are shown in binary units', () => {
  assert.equal(formatBytes(0), '0 bytes');
  assert.equal(formatBytes(1023), '1023 bytes');
  assert.equal(formatBytes(1536), '1.5 KiB');
  assert.equal(formatBytes(4210688), '4.0 MiB');
  assert.equal(formatBytes(3 * 1024 ** 3), '3.0 GiB');
  assert.equal(formatBytes(5000 * 1024 ** 3), '5000.0 GiB');
});

test('the install summary says what Install would do', () => {
  assert.equal(installSummary(ready), 'Not installed. Version 2: 3 files to download (1.5 KiB).');
  assert.equal(installAction(ready), 'Install');
  const current = { ...ready, installedVersion: '2', toDownload: 0 };
  assert.equal(installSummary(current), 'Installed, version 2. Up to date.');
  assert.equal(installAction(current), null);
  const update = { ...ready, installedVersion: '1', toDownload: 1, bytes: 10, toRemove: 2 };
  assert.equal(
    installSummary(update),
    'Installed, version 1. Update to 2: 1 file to download (10 bytes), 2 files to remove.',
  );
  assert.equal(installAction(update), 'Update');
  assert.equal(installAction({ ...ready, installedVersion: '1', toDownload: 0, toRemove: 1 }), 'Update');
  const relink = { ...ready, installedVersion: '2', toDownload: 0, bytes: 0, toPlace: 12 };
  assert.equal(installSummary(relink), 'Installed, version 2. Update to 2: 0 files to download (0 bytes), 12 game files to place.');
  assert.equal(installAction(relink), 'Update');
  assert.equal(installSummary({ kind: 'unavailable', reason: 'no Steam' }), 'Cannot install: no Steam');
  assert.equal(installAction({ kind: 'unavailable', reason: 'no Steam' }), null);
  assert.match(installSummary({ kind: 'refused', reason: 'bad path' }), /refused: bad path$/);
  assert.equal(installAction({ kind: 'refused', reason: 'bad path' }), null);
});

test('each phase has a text and a measure where one exists', () => {
  assert.equal(phaseText({ phase: 'planning' }), 'Checking files');
  assert.equal(phaseFraction({ phase: 'planning' }), null);
  assert.equal(phaseText({ phase: 'removing', done: 1, total: 4 }), 'Removing files (1 of 4)');
  assert.equal(phaseFraction({ phase: 'removing', done: 1, total: 4 }), 0.25);
  assert.equal(phaseText({ phase: 'linking', done: 120, total: 316 }), 'Linking game files (120 of 316)');
  assert.equal(phaseFraction({ phase: 'linking', done: 0, total: 0 }), 1);
  const downloading = {
    phase: /** @type {const} */ ('downloading'),
    file: 'Data/x.esp',
    done: 1,
    total: 7,
    bytesDone: 512,
    bytesTotal: 2048,
  };
  assert.equal(phaseText(downloading), 'Downloading Data/x.esp (file 2 of 7, 512 bytes of 2.0 KiB)');
  assert.equal(phaseFraction(downloading), 0.25);
  assert.equal(phaseText({ phase: 'finishing' }), 'Finishing');
  assert.equal(phaseFraction({ phase: 'finishing' }), null);
});

test('the outcome text covers every ending and nothing else', () => {
  assert.equal(outcomeText({ state: 'idle' }, 'x'), null);
  assert.equal(outcomeText({ state: 'running', id: 'x', phase: { phase: 'planning' } }, 'x'), null);
  const report = { downloaded: 2, kept: 1, removed: 0, linked: 315, copied: 0, notices: ['installed.json was renamed'] };
  assert.equal(
    outcomeText({ state: 'done', id: 'x', report }, 'x'),
    'Files are in place: 2 downloaded, 1 kept, 0 removed, 315 linked, 0 copied.\ninstalled.json was renamed',
  );
  assert.equal(outcomeText({ state: 'failed', id: 'x', message: 'no space' }, 'x'), 'The install failed: no space');
  assert.match(outcomeText({ state: 'cancelled', id: 'x' }, 'x') ?? '', /^The install was cancelled/);
});

test('the account line covers every sign-in state', () => {
  assert.match(accountText({ kind: 'signedOut' }), /^Not signed in/);
  assert.equal(accountText({ kind: 'signedIn', displayName: 'Tester', access: 'ok' }), 'Signed in as Tester. You may play.');
  assert.match(accountText({ kind: 'signedIn', displayName: 'Tester', access: 'not_member' }), /not a member/);
  assert.match(accountText({ kind: 'signedIn', displayName: 'Tester', access: 'missing_role' }), /no role/);
  assert.match(accountText({ kind: 'signedIn', displayName: 'Tester', access: 'banned' }), /banned/);
  assert.match(accountText({ kind: 'signedIn', displayName: 'Tester', access: 'discord_unavailable' }), /cannot reach Discord/);
  const forever = accountText({ kind: 'banned', displayName: 'Tester', banUntil: null, banReason: null });
  assert.equal(forever, 'Tester is banned from this server.');
  const until = accountText({ kind: 'banned', displayName: 'Tester', banUntil: 1760000000000, banReason: 'test ban' });
  assert.match(until, /^Tester is banned from this server until .+\. Reason: test ban$/);
  assert.match(
    accountText({ kind: 'unchecked', displayName: 'Tester', reason: 'the server could not be reached' }),
    /^Signed in as Tester, but .*could not be reached$/,
  );
});

test('the sign-in progress line names waiting, done and failed, and other servers', () => {
  assert.equal(signInProgressText({ state: 'idle' }, 'x'), null);
  assert.match(signInProgressText({ state: 'waiting', id: 'x' }, 'x') ?? '', /^Waiting for the browser/);
  assert.match(signInProgressText({ state: 'waiting', id: 'other' }, 'x') ?? '', /^A sign-in for other is waiting/);
  assert.equal(signInProgressText({ state: 'done', id: 'x', displayName: 'Tester' }, 'x'), 'Signed in as Tester.');
  assert.equal(signInProgressText({ state: 'failed', id: 'x', message: 'refused' }, 'x'), 'The sign-in failed: refused');
  assert.equal(
    signInProgressText({ state: 'failed', id: 'other', message: 'refused' }, 'x'),
    'other: The sign-in failed: refused',
  );
});

test('the play phase, notice and buttons follow the run', () => {
  const idle = { state: /** @type {const} */ ('idle'), id: null, notice: null };
  assert.equal(playPhaseText(idle, 'x'), null);
  assert.deepEqual(playButtons(idle, 'x'), { play: true, reconnect: false });
  const verifying = { ...idle, state: /** @type {const} */ ('verifyingFiles'), id: 'x' };
  assert.equal(playPhaseText(verifying, 'x'), "Checking the server's files.");
  assert.deepEqual(playButtons(verifying, 'x'), { play: false, reconnect: false });
  assert.equal(playPhaseText({ ...verifying, state: 'requestingToken' }, 'x'), 'Asking the server for a play token.');
  assert.equal(playPhaseText({ ...verifying, state: 'launching' }, 'x'), 'Starting the game.');
  const running = { ...idle, state: /** @type {const} */ ('running'), id: 'x' };
  assert.match(playPhaseText(running, 'x') ?? '', /^The game is running\. Keep the launcher open/);
  assert.deepEqual(playButtons(running, 'x'), { play: false, reconnect: true });
  assert.match(playPhaseText(running, 'other') ?? '', /^The game is running for x\./);
  assert.deepEqual(playButtons(running, 'other'), { play: false, reconnect: false });
  const reconnecting = { ...running, state: /** @type {const} */ ('reconnecting') };
  assert.equal(playPhaseText(reconnecting, 'x'), 'Asking the server for a new play token.');
  assert.deepEqual(playButtons(reconnecting, 'x'), { play: false, reconnect: false });
  assert.equal(playNoticeText({ kind: 'failed', message: 'the game server is full' }), 'Play failed: the game server is full');
  assert.match(playNoticeText({ kind: 'reconnected' }), /new play token/);
  assert.equal(playNoticeText({ kind: 'exited', code: 0, problem: null }), 'The game exited.');
  assert.equal(playNoticeText({ kind: 'exited', code: null, problem: null }), 'The game exited.');
  assert.equal(playNoticeText({ kind: 'exited', code: 3, problem: null }), 'The game exited with code 3.');
  assert.equal(
    playNoticeText({ kind: 'exited', code: 0, problem: 'plugins.txt is held open' }),
    'The game exited. Its files could not be put back (plugins.txt is held open); the next launcher start puts them back.',
  );
});

test('an outcome for another server is named after it', () => {
  assert.equal(
    outcomeText({ state: 'failed', id: 'other.example.org', message: 'no space' }, 'x'),
    'other.example.org: The install failed: no space',
  );
  assert.match(
    outcomeText({ state: 'cancelled', id: 'other.example.org' }, 'x') ?? '',
    /^other\.example\.org: The install was cancelled/,
  );
  const report = { downloaded: 0, kept: 0, removed: 0, linked: 0, copied: 0, notices: [] };
  assert.equal(
    outcomeText({ state: 'done', id: 'other.example.org', report }, 'x'),
    'other.example.org: Files are in place: 0 downloaded, 0 kept, 0 removed, 0 linked, 0 copied.',
  );
});

test('the update notice says only why a check was skipped', () => {
  assert.equal(updateNotice({ kind: 'upToDate' }), null);
  assert.equal(updateNotice({ kind: 'available', version: '0.2.0' }), null);
  assert.equal(
    updateNotice({ kind: 'skipped', reason: 'the update host could not be reached' }),
    'Updates could not be checked: the update host could not be reached',
  );
});

test('the update text and bar follow the download and the installer', () => {
  assert.match(updateText({ state: 'idle' }, '0.2.0'), /^Version 0\.2\.0 is available/);
  assert.equal(updateFraction({ state: 'idle' }), null);
  const sized = { state: /** @type {const} */ ('downloading'), bytesDone: 512, bytesTotal: 2048 };
  assert.equal(updateText(sized, '0.2.0'), 'Downloading version 0.2.0 (512 bytes of 2.0 KiB)');
  assert.equal(updateFraction(sized), 0.25);
  const unsized = { ...sized, bytesTotal: null };
  assert.equal(updateText(unsized, '0.2.0'), 'Downloading version 0.2.0 (512 bytes)');
  assert.equal(updateFraction(unsized), null);
  assert.equal(updateFraction({ ...sized, bytesTotal: 0 }), null);
  assert.match(updateText({ state: 'installing' }, '0.2.0'), /^Starting the installer for version 0\.2\.0\./);
  assert.equal(updateFraction({ state: 'installing' }), 1);
  assert.equal(
    updateText({ state: 'failed', message: 'the signature of the update is not valid' }, '0.2.0'),
    'The update to version 0.2.0 failed: the signature of the update is not valid',
  );
  assert.equal(updateFraction({ state: 'failed', message: 'x' }), null);
});
