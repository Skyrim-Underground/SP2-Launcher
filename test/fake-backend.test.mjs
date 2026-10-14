// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { startServer } from './spawn.mjs';

/**
 * A stand-in for the launcher's loopback listener: records the one callback it receives.
 * @returns {Promise<{ port: number, received: () => Promise<URL>, close: () => void }>}
 */
function loopbackListener() {
  return new Promise((resolve) => {
    /** @type {(url: URL) => void} */
    let deliver = () => {};
    const received = new Promise((resolveUrl) => {
      deliver = resolveUrl;
    });
    const server = createServer((request, response) => {
      deliver(new URL(request.url ?? '/', 'http://127.0.0.1'));
      response.writeHead(200, { 'content-type': 'text/plain' });
      response.end('ok');
    });
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      const port = typeof address === 'object' && address ? address.port : 0;
      resolve({ port, received: () => received, close: () => server.close() });
    });
  });
}

/** @param {string} verifier */
function challengeOf(verifier) {
  return createHash('sha256').update(verifier).digest('base64url');
}

/**
 * The whole flow as the launcher and the browser would run it, against one backend.
 * @param {string} origin
 * @returns {Promise<{ session: string, displayName: string }>}
 */
async function signIn(origin) {
  const listener = await loopbackListener();
  try {
    const verifier = randomBytes(32).toString('base64url');
    const started = await fetch(`${origin}/api/v1/users/login-discord/start`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ challenge: challengeOf(verifier), port: listener.port }),
    });
    assert.equal(started.status, 200);
    const { state, authorizeUrl } = await started.json();
    assert.match(authorizeUrl, /^https:\/\/discord\.com\/oauth2\/authorize\?/);
    assert.equal(new URL(authorizeUrl).searchParams.get('state'), state);
    const redirect = await fetch(`${origin}/api/v1/users/login-discord/callback?code=discord-code&state=${state}`, {
      redirect: 'manual',
    });
    assert.equal(redirect.status, 302);
    const location = redirect.headers.get('location') ?? '';
    assert.ok(location.startsWith(`http://127.0.0.1:${listener.port}/callback?`), location);
    await fetch(location);
    const callback = await listener.received();
    assert.equal(callback.searchParams.get('state'), state);
    const code = callback.searchParams.get('code') ?? '';
    const redeemed = await fetch(`${origin}/api/v1/users/login-discord/redeem`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ state, code, verifier }),
    });
    assert.equal(redeemed.status, 200);
    const body = await redeemed.json();
    assert.match(body.session, /^sugs_[A-Za-z0-9_-]{43}$/);
    assert.ok(body.expiresAt > Date.now());
    return { session: body.session, displayName: body.displayName };
  } finally {
    listener.close();
  }
}

/**
 * @param {string} origin
 * @param {string} path
 * @param {string} session
 * @param {string} method
 */
function withSession(origin, path, session, method) {
  return fetch(`${origin}${path}`, {
    method,
    headers: { authorization: `Bearer ${session}`, 'content-type': 'application/json' },
    body: method === 'POST' ? '{}' : undefined,
  });
}

test('the server route carries the configured API version', async () => {
  const backend = await startServer('fake-backend.mjs', ['--api-version', '2', '--name', 'Future']);
  try {
    const response = await fetch(`${backend.origin}/api/v1/server`);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('cache-control'), 'public, max-age=60');
    const body = await response.json();
    assert.equal(body.apiVersion, 2);
    assert.equal(body.name, 'Future');
    assert.equal(body.manifestUrl, '/api/v1/manifest');
  } finally {
    backend.stop();
  }
});

test('status and manifest follow the backend contract', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'sug-launcher-fake-'));
  const manifest = join(dir, 'manifest.json');
  await writeFile(manifest, '{"schemaVersion":1}');
  const backend = await startServer('fake-backend.mjs', ['--manifest', manifest]);
  try {
    const status = await (await fetch(`${backend.origin}/api/v1/status`)).json();
    assert.deepEqual(status, { online: true, players: 3, maxPlayers: 64, signIn: 'ok' });
    const served = await fetch(`${backend.origin}/api/v1/manifest`);
    assert.equal(served.status, 200);
    assert.equal(await served.text(), '{"schemaVersion":1}');
    const missing = await fetch(`${backend.origin}/api/v1/other`);
    assert.equal(missing.status, 404);
  } finally {
    backend.stop();
  }
});

test('without a manifest file the manifest route answers 503, and offline reports no players', async () => {
  const backend = await startServer('fake-backend.mjs', ['--offline']);
  try {
    const manifest = await fetch(`${backend.origin}/api/v1/manifest`);
    assert.equal(manifest.status, 503);
    assert.deepEqual(await manifest.json(), { error: 'manifest_unavailable' });
    const status = await (await fetch(`${backend.origin}/api/v1/status`)).json();
    assert.equal(status.online, false);
    assert.equal(status.players, 0);
  } finally {
    backend.stop();
  }
});

test('the sign-in flow yields a session that me, play and logout accept', async () => {
  const backend = await startServer('fake-backend.mjs', ['--display-name', 'Tester']);
  try {
    const { session, displayName } = await signIn(backend.origin);
    assert.equal(displayName, 'Tester');
    const me = await withSession(backend.origin, '/api/v1/users/me', session, 'GET');
    assert.equal(me.status, 200);
    assert.deepEqual(await me.json(), { displayName: 'Tester', access: 'ok' });
    const play = await withSession(backend.origin, '/api/v1/play', session, 'POST');
    assert.equal(play.status, 200);
    const grant = await play.json();
    assert.match(grant.playToken, /^sugp_[A-Za-z0-9_-]{43}$/);
    assert.deepEqual(grant.server, { host: 'game.example.org', port: 7777 });
    const logout = await withSession(backend.origin, '/api/v1/users/logout', session, 'POST');
    assert.equal(logout.status, 204);
    const gone = await withSession(backend.origin, '/api/v1/users/me', session, 'GET');
    assert.equal(gone.status, 401);
    assert.deepEqual(await gone.json(), { error: 'unauthorized' });
    const anonymous = await fetch(`${backend.origin}/api/v1/play`, { method: 'POST', body: '{}' });
    assert.equal(anonymous.status, 401);
  } finally {
    backend.stop();
  }
});

test('the log names the Authorization header of every request', async () => {
  const backend = await startServer('fake-backend.mjs', []);
  try {
    const { session } = await signIn(backend.origin);
    await withSession(backend.origin, '/api/v1/users/me', session, 'GET');
    const log = await (await fetch(`${backend.origin}/fake/log`)).json();
    const withHeader = log.filter((entry) => entry.authorization !== null);
    assert.deepEqual(withHeader, [{ method: 'GET', path: '/api/v1/users/me', authorization: `Bearer ${session}` }]);
    assert.equal(log[0].path, '/api/v1/users/login-discord/start');
    assert.equal(log[0].authorization, null);
  } finally {
    backend.stop();
  }
});

test('redeem refuses a wrong verifier, a wrong state, a reused code and an unknown state at the callback', async () => {
  const backend = await startServer('fake-backend.mjs', []);
  const listener = await loopbackListener();
  try {
    const verifier = randomBytes(32).toString('base64url');
    const started = await fetch(`${backend.origin}/api/v1/users/login-discord/start`, {
      method: 'POST',
      body: JSON.stringify({ challenge: challengeOf(verifier), port: listener.port }),
    });
    const { state } = await started.json();
    const redirect = await fetch(`${backend.origin}/api/v1/users/login-discord/callback?code=x&state=${state}`, {
      redirect: 'manual',
    });
    const code = new URL(redirect.headers.get('location') ?? '').searchParams.get('code') ?? '';
    const redeem = (/** @type {Record<string, string>} */ body) =>
      fetch(`${backend.origin}/api/v1/users/login-discord/redeem`, { method: 'POST', body: JSON.stringify(body) });
    const wrongState = await redeem({ state: randomBytes(32).toString('base64url'), code, verifier });
    assert.equal(wrongState.status, 400);
    assert.deepEqual(await wrongState.json(), { error: 'invalid' });
    // The code was spent by the first attempt, so the right verifier comes too late.
    const spent = await redeem({ state, code, verifier });
    assert.equal(spent.status, 400);
    const again = await fetch(`${backend.origin}/api/v1/users/login-discord/callback?code=x&state=${state}`, {
      redirect: 'manual',
    });
    assert.equal(again.status, 400);
    assert.equal(again.headers.get('content-security-policy'), "default-src 'none'");
    const malformed = await redeem({ state, code: 'short', verifier });
    assert.equal(malformed.status, 400);
    assert.deepEqual(await malformed.json(), { error: 'bad_request' });
  } finally {
    listener.close();
    backend.stop();
  }
});

test('play and me follow the configured access and server state', async () => {
  for (const [args, status, error] of [
    [['--access', 'banned', '--ban-reason', 'test ban'], 403, 'banned'],
    [['--access', 'missing_role'], 403, 'missing_role'],
    [['--access', 'discord_unavailable'], 503, 'discord_unavailable'],
    [['--offline'], 503, 'server_offline'],
    [['--full'], 503, 'server_full'],
  ]) {
    const backend = await startServer('fake-backend.mjs', args);
    try {
      const { session } = await signIn(backend.origin);
      const play = await withSession(backend.origin, '/api/v1/play', session, 'POST');
      assert.equal(play.status, status, args.join(' '));
      assert.deepEqual(await play.json(), { error });
      const me = await (await withSession(backend.origin, '/api/v1/users/me', session, 'GET')).json();
      if (error === 'banned') {
        assert.equal(me.access, 'banned');
        assert.equal(me.banReason, 'test ban');
        assert.ok(me.banUntil > Date.now());
      } else if (args[0] === '--access') {
        assert.equal(me.access, error);
      } else {
        assert.equal(me.access, 'ok');
      }
    } finally {
      backend.stop();
    }
  }
});

test('a callback error reaches the launcher instead of a code', async () => {
  const backend = await startServer('fake-backend.mjs', ['--callback-error', 'not_member']);
  const listener = await loopbackListener();
  try {
    const started = await fetch(`${backend.origin}/api/v1/users/login-discord/start`, {
      method: 'POST',
      body: JSON.stringify({ challenge: challengeOf('x'.repeat(43)), port: listener.port }),
    });
    const { state } = await started.json();
    const redirect = await fetch(`${backend.origin}/api/v1/users/login-discord/callback?code=x&state=${state}`, {
      redirect: 'manual',
    });
    const location = new URL(redirect.headers.get('location') ?? '');
    assert.equal(location.searchParams.get('error'), 'not_member');
    assert.equal(location.searchParams.get('code'), null);
    assert.equal(location.searchParams.get('state'), state);
  } finally {
    listener.close();
    backend.stop();
  }
});

test('two instances keep separate sessions', async () => {
  const first = await startServer('fake-backend.mjs', ['--display-name', 'One']);
  const second = await startServer('fake-backend.mjs', ['--display-name', 'Two']);
  try {
    const a = await signIn(first.origin);
    const b = await signIn(second.origin);
    assert.notEqual(a.session, b.session);
    assert.equal((await withSession(first.origin, '/api/v1/users/me', a.session, 'GET')).status, 200);
    assert.equal((await withSession(second.origin, '/api/v1/users/me', b.session, 'GET')).status, 200);
    assert.equal((await withSession(first.origin, '/api/v1/users/me', b.session, 'GET')).status, 401);
    assert.equal((await withSession(second.origin, '/api/v1/users/me', a.session, 'GET')).status, 401);
  } finally {
    first.stop();
    second.stop();
  }
});
