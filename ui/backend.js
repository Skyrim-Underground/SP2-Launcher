// SPDX-License-Identifier: AGPL-3.0-or-later
// Typed wrappers over the launcher's own commands; everything crossing here is a plain JSON contract.

/** @typedef {{ id: string, name: string, backendUrl: string, listed: boolean, addedAt: number }} ServerEntry */
/** @typedef {{ state: 'fresh' } | { state: 'cached', reason: string } | { state: 'unavailable', reason: string } | { state: 'unconfigured' }} DirectoryReport */
/** @typedef {{ online: boolean, players: number, maxPlayers: number, signIn: string }} Status */
/** @typedef {{ id: string, status: Status | null }} StatusRow */
/** @typedef {{ name: string, description: string, website: string, discordInvite: string, protocolId: string, gameRuntime: string, apiVersion: number, manifestUrl: string }} ServerInfo */
/** @typedef {{ kind: 'ok', info: ServerInfo } | { kind: 'launcherTooOld', apiVersion: number } | { kind: 'serverTooOld', apiVersion: number }} ServerCheck */
/** @typedef {{ entry: ServerEntry, check: ServerCheck | null }} ServerRow */
/** @typedef {{ servers: ServerRow[], directory: DirectoryReport | null, setAside: string[] }} ServerList */
/** @typedef {{ entry: ServerEntry, check: ServerCheck }} ServerPage */
/** @typedef {{ origin: string, name: string, description: string, website: string, warning: string }} AddPreview */
/** @typedef {{ baseFolder: string | null, defaultBaseFolder: string | null }} Settings */
/** @typedef {{ version: string, website: string, rulesUrl: string }} LauncherInfo */
/** @typedef {{ to: string, origin: string }} NativeFile */
/** @typedef {{ kind: 'unavailable', reason: string } | { kind: 'refused', reason: string } | { kind: 'ready', version: string, installedVersion: string | null, native: NativeFile[], toDownload: number, bytes: number, toRemove: number, toPlace: number, gameFolder: string }} InstallState */
/** @typedef {{ phase: 'planning' } | { phase: 'removing', done: number, total: number } | { phase: 'linking', done: number, total: number } | { phase: 'downloading', file: string, done: number, total: number, bytesDone: number, bytesTotal: number } | { phase: 'finishing' }} Phase */
/** @typedef {{ downloaded: number, kept: number, removed: number, linked: number, copied: number, notices: string[] }} Report */
/** @typedef {{ state: 'idle' } | { state: 'running', id: string, phase: Phase } | { state: 'done', id: string, report: Report } | { state: 'failed', id: string, message: string } | { state: 'cancelled', id: string }} InstallProgress */
/** @typedef {'ok' | 'not_member' | 'missing_role' | 'banned' | 'discord_unavailable'} Access */
/** @typedef {{ kind: 'signedOut' } | { kind: 'signedIn', displayName: string, access: Access } | { kind: 'banned', displayName: string, banUntil: number | null, banReason: string | null } | { kind: 'unchecked', displayName: string, reason: string }} SignInView */
/** @typedef {{ state: 'idle' } | { state: 'waiting', id: string } | { state: 'done', id: string, displayName: string } | { state: 'failed', id: string, message: string }} SignInProgress */
/** @typedef {'idle' | 'verifyingFiles' | 'requestingToken' | 'launching' | 'running' | 'reconnecting'} PlayState */
/** @typedef {{ kind: 'failed', message: string } | { kind: 'reconnected' } | { kind: 'exited', code: number | null, problem: string | null }} PlayNotice */
/** @typedef {{ state: PlayState, id: string | null, notice: PlayNotice | null }} PlayView */
/** @typedef {{ kind: 'upToDate' } | { kind: 'available', version: string } | { kind: 'skipped', reason: string }} UpdateCheck */
/** @typedef {{ state: 'idle' } | { state: 'downloading', bytesDone: number, bytesTotal: number | null } | { state: 'installing' } | { state: 'failed', message: string }} UpdateProgress */

/**
 * @template T
 * @param {string} command
 * @param {Record<string, unknown>} [args]
 * @returns {Promise<T>}
 */
function invoke(command, args) {
  return window.__TAURI__.core.invoke(command, args);
}

/** @returns {Promise<LauncherInfo>} */
export function launcherInfo() {
  return invoke('launcher_info');
}

/**
 * The check at start; nothing else runs until an available update is installed or has failed.
 * @returns {Promise<UpdateCheck>}
 */
export function updateCheck() {
  return invoke('update_check');
}

/**
 * Downloads and installs the pending update in the background; once the installer runs, the launcher exits.
 * @returns {Promise<void>}
 */
export function updateInstall() {
  return invoke('update_install');
}

/** @returns {Promise<UpdateProgress>} */
export function updateProgress() {
  return invoke('update_progress');
}

/**
 * Refreshes the directory and every server's API version.
 * @returns {Promise<ServerList>}
 */
export function loadServers() {
  return invoke('load_servers');
}

/**
 * The list as the last refresh left it.
 * @returns {Promise<ServerList>}
 */
export function listServers() {
  return invoke('list_servers');
}

/** @returns {Promise<StatusRow[]>} */
export function serverStatus() {
  return invoke('server_status');
}

/**
 * @param {string} id
 * @returns {Promise<ServerPage>}
 */
export function serverPage(id) {
  return invoke('server_page', { id });
}

/**
 * Fetches the server's manifest and plans an install without running it.
 * @param {string} id
 * @returns {Promise<InstallState>}
 */
export function installState(id) {
  return invoke('install_state', { id });
}

/**
 * Starts an install in the background; `verify` hashes every file in full.
 * @param {string} id
 * @param {boolean} verify
 * @returns {Promise<void>}
 */
export function installStart(id, verify) {
  return invoke('install_start', { id, verify });
}

/**
 * What the running install is doing, or how the last one ended; an ending is reported once.
 * @returns {Promise<InstallProgress>}
 */
export function installProgress() {
  return invoke('install_progress');
}

/** @returns {Promise<void>} */
export function installCancel() {
  return invoke('install_cancel');
}

/**
 * @param {string} url
 * @returns {Promise<AddPreview>}
 */
export function previewServer(url) {
  return invoke('preview_server', { url });
}

/**
 * @param {string} url
 * @returns {Promise<ServerEntry>}
 */
export function addServer(url) {
  return invoke('add_server', { url });
}

/**
 * @param {string} id
 * @returns {Promise<void>}
 */
export function removeServer(id) {
  return invoke('remove_server', { id });
}

/**
 * @param {string} url
 * @returns {Promise<void>}
 */
export function openLink(url) {
  return invoke('open_link', { url });
}

/** @returns {Promise<Settings>} */
export function getSettings() {
  return invoke('get_settings');
}

/**
 * @param {string} folder
 * @returns {Promise<Settings>}
 */
export function setBaseFolder(folder) {
  return invoke('set_base_folder', { folder });
}

/**
 * Asks the server whether the stored sign-in still allows play.
 * @param {string} id
 * @returns {Promise<SignInView>}
 */
export function signInState(id) {
  return invoke('signin_state', { id });
}

/**
 * Starts a sign-in and opens the browser; the rest runs until the browser comes back.
 * @param {string} id
 * @returns {Promise<void>}
 */
export function signInStart(id) {
  return invoke('signin_start', { id });
}

/**
 * Where the one sign-in stands; an ending is reported once.
 * @returns {Promise<SignInProgress>}
 */
export function signInProgress() {
  return invoke('signin_progress');
}

/** @returns {Promise<void>} */
export function signInCancel() {
  return invoke('signin_cancel');
}

/**
 * @param {string} id
 * @returns {Promise<void>}
 */
export function signOut(id) {
  return invoke('sign_out', { id });
}

/**
 * Checks the files, asks for a play token and starts the game; resolves once the game runs or the start failed.
 * @param {string} id
 * @returns {Promise<PlayView>}
 */
export function play(id) {
  return invoke('play', { id });
}

/** @returns {Promise<PlayView>} */
export function playState() {
  return invoke('play_state');
}

/**
 * A new play token for the running game.
 * @param {string} id
 * @returns {Promise<PlayView>}
 */
export function reconnect(id) {
  return invoke('reconnect', { id });
}
