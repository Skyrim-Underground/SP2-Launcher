// SPDX-License-Identifier: AGPL-3.0-or-later
// A stand-in for a backend's launcher-facing routes under /api/v1/: server, status, manifest, the Discord sign-in (start, callback, redeem), me, logout and play.
// Discord itself is played by the test, which calls the callback with any code.
// Prints "listening http://127.0.0.1:<port>" once it accepts connections.
import { createHash, randomBytes } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { parseArgs } from 'node:util';

const { values: options } = parseArgs({
  options: {
    port: { type: 'string', default: '0' },
    'api-version': { type: 'string', default: '1' },
    name: { type: 'string', default: 'Fake Server' },
    offline: { type: 'boolean', default: false },
    full: { type: 'boolean', default: false },
    manifest: { type: 'string' },
    // What /api/v1/users/me and /api/v1/play say about the account: ok, not_member, missing_role, banned or discord_unavailable.
    access: { type: 'string', default: 'ok' },
    // When set, the sign-in callback sends the launcher this error instead of a login code.
    'callback-error': { type: 'string' },
    // The sign-in address the start route answers with; the launcher must refuse anything but Discord's.
    'authorize-url': { type: 'string', default: 'https://discord.com/oauth2/authorize' },
    'display-name': { type: 'string', default: 'Fake Player' },
    'game-host': { type: 'string', default: 'game.example.org' },
    'game-port': { type: 'string', default: '7777' },
    'ban-reason': { type: 'string' },
  },
});

const apiVersion = Number(options['api-version']);
if (!Number.isInteger(apiVersion) || apiVersion < 0) {
  console.error('--api-version must be a non-negative integer');
  process.exit(2);
}
const ACCESS_VALUES = ['ok', 'not_member', 'missing_role', 'banned', 'discord_unavailable'];
if (!ACCESS_VALUES.includes(options.access)) {
  console.error(`--access must be one of ${ACCESS_VALUES.join(', ')}`);
  process.exit(2);
}

const CACHEABLE = 'public, max-age=60';
const NO_STORE = 'no-store';
const BODY_MAX = 4096;
const VALUE = /^[A-Za-z0-9_-]{43}$/;
const SESSION_PATTERN = /^sugs_[A-Za-z0-9_-]{43}$/;
const PORT_MIN = 1024;
const PORT_MAX = 65535;
const FLOW_TTL_MS = 10 * 60 * 1000;
const CODE_TTL_MS = 120_000;
const SESSION_TTL_MS = 30 * 24 * 60 * 60 * 1000;
const PLAY_TOKEN_TTL_MS = 300_000;
const MAX_PLAYERS = 64;
const FORBIDDEN = ['banned', 'not_member', 'missing_role'];
const ERROR_PAGE = '<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Sign-in failed</title></head><body><h1>Sign-in failed</h1></body></html>';

const server = {
  name: options.name,
  description: 'A fake server for launcher tests',
  website: 'https://www.example.org',
  discordInvite: 'https://discord.example.org/fake',
  protocolId: 'sug1_',
  gameRuntime: 'skyrimse-1.7.104',
  apiVersion,
  manifestUrl: '/api/v1/manifest',
};

const players = options.offline ? 0 : options.full ? MAX_PLAYERS : 3;
const status = {
  online: !options.offline,
  players,
  maxPlayers: MAX_PLAYERS,
  signIn: 'ok',
};

/** @type {Map<string, { challenge: string, port: number, expiresAt: number }>} */
const flows = new Map();
/** @type {Map<string, { state: string, challenge: string, expiresAt: number }>} */
const codes = new Map();
/** @type {Map<string, { expiresAt: number }>} */
const sessions = new Map();
/** Every request, with the Authorization header it carried, for tests that check where a session was sent. */
/** @type {{ method: string, path: string, authorization: string | null }[]} */
const log = [];

/** @param {number} bytes */
function random(bytes) {
  return randomBytes(bytes).toString('base64url');
}

/** @param {string} verifier */
function challengeOf(verifier) {
  return createHash('sha256').update(verifier).digest('base64url');
}

/**
 * @param {import('node:http').ServerResponse} response
 * @param {number} code
 * @param {unknown} body
 * @param {string} cacheControl
 */
function sendJson(response, code, body, cacheControl) {
  const bytes = Buffer.from(JSON.stringify(body), 'utf8');
  response.writeHead(code, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': bytes.length,
    'cache-control': cacheControl,
  });
  response.end(bytes);
}

/**
 * @param {import('node:http').ServerResponse} response
 * @param {number} code
 */
function sendEmpty(response, code) {
  response.writeHead(code, { 'cache-control': NO_STORE, 'content-length': 0 });
  response.end();
}

/** @param {import('node:http').ServerResponse} response */
async function sendManifest(response) {
  if (!options.manifest) {
    sendJson(response, 503, { error: 'manifest_unavailable' }, NO_STORE);
    return;
  }
  let bytes;
  try {
    bytes = await readFile(options.manifest);
    JSON.parse(bytes.toString('utf8'));
  } catch {
    sendJson(response, 503, { error: 'manifest_unavailable' }, NO_STORE);
    return;
  }
  response.writeHead(200, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': bytes.length,
    'cache-control': CACHEABLE,
  });
  response.end(bytes);
}

/**
 * The parsed JSON body, or undefined when it is missing, too large or not an object.
 * @param {import('node:http').IncomingMessage} request
 * @returns {Promise<Record<string, unknown> | undefined>}
 */
async function readJson(request) {
  const chunks = [];
  let length = 0;
  for await (const chunk of request) {
    length += chunk.length;
    if (length > BODY_MAX) return undefined;
    chunks.push(chunk);
  }
  try {
    const parsed = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    return typeof parsed === 'object' && parsed !== null && !Array.isArray(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

/**
 * True when the body holds exactly the named keys.
 * @param {Record<string, unknown>} body
 * @param {string[]} keys
 */
function hasExactly(body, keys) {
  const present = Object.keys(body);
  return present.length === keys.length && keys.every((key) => present.includes(key));
}

/**
 * The live session named by the Authorization header, or undefined.
 * @param {import('node:http').IncomingMessage} request
 */
function sessionOf(request) {
  const header = request.headers.authorization;
  if (typeof header !== 'string' || !header.startsWith('Bearer ')) return undefined;
  const token = header.slice('Bearer '.length);
  if (!SESSION_PATTERN.test(token)) return undefined;
  const session = sessions.get(token);
  if (session === undefined) return undefined;
  if (Date.now() >= session.expiresAt) {
    sessions.delete(token);
    return undefined;
  }
  return token;
}

/**
 * @param {import('node:http').IncomingMessage} request
 * @param {import('node:http').ServerResponse} response
 */
async function start(request, response) {
  const body = await readJson(request);
  if (
    body === undefined ||
    !hasExactly(body, ['challenge', 'port']) ||
    typeof body.challenge !== 'string' ||
    !VALUE.test(body.challenge) ||
    !Number.isInteger(body.port) ||
    Number(body.port) < PORT_MIN ||
    Number(body.port) > PORT_MAX
  ) {
    sendJson(response, 400, { error: 'bad_request' }, NO_STORE);
    return;
  }
  const state = random(32);
  flows.set(state, { challenge: body.challenge, port: Number(body.port), expiresAt: Date.now() + FLOW_TTL_MS });
  const url = new URL(options['authorize-url']);
  url.search = new URLSearchParams({
    client_id: '000000000000000000',
    response_type: 'code',
    scope: 'identify',
    redirect_uri: `${origin()}/api/v1/users/login-discord/callback`,
    state,
    prompt: 'none',
    code_challenge: challengeOf(random(32)),
    code_challenge_method: 'S256',
  }).toString();
  sendJson(response, 200, { state, authorizeUrl: url.toString(), expiresIn: FLOW_TTL_MS / 1000 }, NO_STORE);
}

/**
 * The browser lands here from Discord (played by the test); it leaves with a redirect to the launcher's loopback listener.
 * @param {URL} url
 * @param {import('node:http').ServerResponse} response
 */
function callback(url, response) {
  const state = url.searchParams.get('state');
  const flow = state === null ? undefined : flows.get(state);
  if (state === null || flow === undefined) {
    response.writeHead(400, {
      'content-type': 'text/html; charset=utf-8',
      'content-security-policy': "default-src 'none'",
      'referrer-policy': 'no-referrer',
      'cache-control': NO_STORE,
    });
    response.end(ERROR_PAGE);
    return;
  }
  flows.delete(state);
  const params = new URLSearchParams({ state });
  const discordCode = url.searchParams.get('code');
  if (Date.now() >= flow.expiresAt || url.searchParams.has('error') || discordCode === null || discordCode === '') {
    params.set('error', 'denied');
  } else if (options['callback-error'] !== undefined) {
    params.set('error', options['callback-error']);
  } else {
    const code = random(32);
    codes.set(code, { state, challenge: flow.challenge, expiresAt: Date.now() + CODE_TTL_MS });
    params.set('code', code);
  }
  response.writeHead(302, {
    location: `http://127.0.0.1:${flow.port}/callback?${params.toString()}`,
    'referrer-policy': 'no-referrer',
    'cache-control': NO_STORE,
    'content-length': 0,
  });
  response.end();
}

/**
 * @param {import('node:http').IncomingMessage} request
 * @param {import('node:http').ServerResponse} response
 */
async function redeem(request, response) {
  const body = await readJson(request);
  if (
    body === undefined ||
    !hasExactly(body, ['state', 'code', 'verifier']) ||
    ![body.state, body.code, body.verifier].every((value) => typeof value === 'string' && VALUE.test(value))
  ) {
    sendJson(response, 400, { error: 'bad_request' }, NO_STORE);
    return;
  }
  const code = String(body.code);
  const pending = codes.get(code);
  // The code is spent by the first attempt, whatever the state or verifier presented with it.
  codes.delete(code);
  if (
    pending === undefined ||
    Date.now() >= pending.expiresAt ||
    pending.state !== body.state ||
    challengeOf(String(body.verifier)) !== pending.challenge
  ) {
    sendJson(response, 400, { error: 'invalid' }, NO_STORE);
    return;
  }
  const session = `sugs_${random(32)}`;
  const expiresAt = Date.now() + SESSION_TTL_MS;
  sessions.set(session, { expiresAt });
  sendJson(response, 200, { session, expiresAt, displayName: options['display-name'] }, NO_STORE);
}

/** @param {import('node:http').ServerResponse} response */
function me(response) {
  const body = { displayName: options['display-name'], access: options.access };
  if (options.access === 'banned' && options['ban-reason'] !== undefined) {
    Object.assign(body, { banUntil: Date.now() + 86_400_000, banReason: options['ban-reason'] });
  }
  sendJson(response, 200, body, NO_STORE);
}

/**
 * @param {import('node:http').IncomingMessage} request
 * @param {import('node:http').ServerResponse} response
 */
async function play(request, response) {
  const body = await readJson(request);
  if (body === undefined || Object.keys(body).length !== 0) {
    sendJson(response, 400, { error: 'bad_request' }, NO_STORE);
    return;
  }
  if (options.access !== 'ok') {
    sendJson(response, FORBIDDEN.includes(options.access) ? 403 : 503, { error: options.access }, NO_STORE);
    return;
  }
  if (!status.online) {
    sendJson(response, 503, { error: 'server_offline' }, NO_STORE);
    return;
  }
  if (status.players >= status.maxPlayers) {
    sendJson(response, 503, { error: 'server_full' }, NO_STORE);
    return;
  }
  sendJson(
    response,
    200,
    {
      playToken: `sugp_${random(32)}`,
      expiresAt: Date.now() + PLAY_TOKEN_TTL_MS,
      server: { host: options['game-host'], port: Number(options['game-port']) },
    },
    NO_STORE,
  );
}

/**
 * @param {import('node:http').IncomingMessage} request
 * @param {import('node:http').ServerResponse} response
 */
async function route(request, response) {
  const method = request.method ?? 'GET';
  const url = new URL(request.url ?? '/', 'http://127.0.0.1');
  const authorization = request.headers.authorization;
  log.push({ method, path: url.pathname, authorization: typeof authorization === 'string' ? authorization : null });
  switch (`${method} ${url.pathname}`) {
    case 'GET /api/v1/server':
      sendJson(response, 200, server, CACHEABLE);
      return;
    case 'GET /api/v1/status':
      sendJson(response, 200, status, NO_STORE);
      return;
    case 'GET /api/v1/manifest':
      await sendManifest(response);
      return;
    case 'POST /api/v1/users/login-discord/start':
      await start(request, response);
      return;
    case 'GET /api/v1/users/login-discord/callback':
      callback(url, response);
      return;
    case 'POST /api/v1/users/login-discord/redeem':
      await redeem(request, response);
      return;
    case 'GET /fake/log':
      sendJson(response, 200, log, NO_STORE);
      return;
    default:
      break;
  }
  const session = sessionOf(request);
  switch (`${method} ${url.pathname}`) {
    case 'GET /api/v1/users/me':
      if (session === undefined) sendJson(response, 401, { error: 'unauthorized' }, NO_STORE);
      else me(response);
      return;
    case 'POST /api/v1/users/logout':
      if (session === undefined) {
        sendJson(response, 401, { error: 'unauthorized' }, NO_STORE);
      } else {
        sessions.delete(session);
        sendEmpty(response, 204);
      }
      return;
    case 'POST /api/v1/play':
      if (session === undefined) sendJson(response, 401, { error: 'unauthorized' }, NO_STORE);
      else await play(request, response);
      return;
    default:
      sendJson(response, 404, { error: 'not_found' }, NO_STORE);
  }
}

const http = createServer((request, response) => {
  void route(request, response);
});

function origin() {
  const address = http.address();
  const port = typeof address === 'object' && address ? address.port : options.port;
  return `http://127.0.0.1:${port}`;
}

http.listen(Number(options.port), '127.0.0.1', () => {
  console.log(`listening ${origin()}`);
});
